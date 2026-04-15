//! Album picker dialog widget.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use tracing::debug;

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::album::AlbumId;

use super::album_row::AlbumRow;
use crate::client::album::AlbumPickerData;

struct DialogInner {
    dialog: adw::Dialog,
    list_box: gtk::ListBox,
    add_button: gtk::Button,
    rows: Vec<AlbumRow>,
    selected_album_id: RefCell<Option<AlbumId>>,
    media_ids: Vec<crate::library::media::MediaId>,
    bus_sender: EventSender,
}

struct NewAlbumWidgets {
    separator: gtk::ListBoxRow,
    row: gtk::ListBoxRow,
    stack: gtk::Stack,
    entry: gtk::Entry,
    create_button: gtk::Button,
}

pub fn present(data: AlbumPickerData, bus_sender: EventSender, parent: &gtk::Widget) {
    let total_selected = data.media_ids.len();
    let is_empty = data.albums.is_empty();

    let dialog = adw::Dialog::builder()
        .title("Add to Album")
        .content_width(400)
        .content_height(500)
        .build();

    let header = adw::HeaderBar::new();

    let cancel_btn = gtk::Button::with_label("Cancel");
    header.pack_start(&cancel_btn);

    let add_button = gtk::Button::with_label("Add to album");
    add_button.add_css_class("suggested-action");
    add_button.set_visible(false);
    header.pack_end(&add_button);

    let search_entry = gtk::SearchEntry::builder()
        .placeholder_text("Search albums")
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .margin_bottom(6)
        .build();

    let list_box = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .build();
    list_box.add_css_class("boxed-list");

    let mut rows: Vec<AlbumRow> = Vec::new();
    for entry in &data.albums {
        let album_row = AlbumRow::new(entry, total_selected);
        list_box.append(&album_row.row);
        rows.push(album_row);
    }

    let new_album = build_new_album_row();
    list_box.append(&new_album.separator);
    list_box.append(&new_album.row);

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    scrolled.set_child(Some(&list_box));

    let (empty_page, empty_create_btn) = build_empty_state();

    let content_stack = gtk::Stack::new();
    content_stack.add_named(&scrolled, Some("list"));
    content_stack.add_named(&empty_page, Some("empty"));
    content_stack.set_visible_child_name(if is_empty { "empty" } else { "list" });

    let content_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    if !is_empty {
        content_box.append(&search_entry);
    }
    content_box.append(&content_stack);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content_box));

    dialog.set_child(Some(&toolbar_view));

    let inner = Rc::new(DialogInner {
        dialog: dialog.clone(),
        list_box,
        add_button: add_button.clone(),
        rows,
        selected_album_id: RefCell::new(None),
        media_ids: data.media_ids,
        bus_sender,
    });

    connect_signals(
        &inner,
        &dialog,
        &cancel_btn,
        &add_button,
        &search_entry,
        &new_album,
        &empty_create_btn,
    );

    dialog.present(Some(parent));
}

fn build_new_album_row() -> NewAlbumWidgets {
    let separator = gtk::ListBoxRow::builder()
        .activatable(false)
        .selectable(false)
        .child(&gtk::Separator::new(gtk::Orientation::Horizontal))
        .build();

    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);

    let label_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .build();
    label_box.append(&gtk::Image::from_icon_name("list-add-symbolic"));
    label_box.append(&gtk::Label::new(Some("New album\u{2026}")));
    stack.add_named(&label_box, Some("label"));

    let entry_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(12)
        .margin_end(12)
        .build();
    let entry = gtk::Entry::builder()
        .placeholder_text("Album name")
        .hexpand(true)
        .activates_default(true)
        .build();
    let create_button = gtk::Button::with_label("Create & add");
    create_button.add_css_class("suggested-action");
    create_button.set_sensitive(false);
    entry_box.append(&entry);
    entry_box.append(&create_button);
    stack.add_named(&entry_box, Some("entry"));

    stack.set_visible_child_name("label");

    let row = gtk::ListBoxRow::builder()
        .child(&stack)
        .activatable(true)
        .build();
    row.set_widget_name("new-album");

    NewAlbumWidgets {
        separator,
        row,
        stack,
        entry,
        create_button,
    }
}

fn build_empty_state() -> (adw::StatusPage, gtk::Button) {
    let page = adw::StatusPage::builder()
        .title("No albums")
        .description("Create your first album to organise your photos")
        .icon_name("folder-pictures-symbolic")
        .build();
    let button = gtk::Button::with_label("New Album\u{2026}");
    button.add_css_class("suggested-action");
    button.add_css_class("pill");
    button.set_halign(gtk::Align::Center);
    page.set_child(Some(&button));

    (page, button)
}

fn connect_signals(
    inner: &Rc<DialogInner>,
    dialog: &adw::Dialog,
    cancel_btn: &gtk::Button,
    add_button: &gtk::Button,
    search_entry: &gtk::SearchEntry,
    new_album: &NewAlbumWidgets,
    empty_create_btn: &gtk::Button,
) {
    let new_album_stack = &new_album.stack;
    let new_album_entry = &new_album.entry;
    let create_add_btn = &new_album.create_button;
    {
        let d = dialog.clone();
        cancel_btn.connect_clicked(move |_| {
            d.close();
        });
    }

    {
        let i = Rc::clone(inner);
        let stack = new_album_stack.clone();
        let entry = new_album_entry.clone();
        inner.list_box.connect_row_activated(move |_, row| {
            if row.widget_name() == "new-album" {
                stack.set_visible_child_name("entry");
                entry.grab_focus();
                return;
            }

            let album_id_str = row.widget_name().to_string();
            debug!(album_id = %album_id_str, "album row activated");

            for r in &i.rows {
                r.set_selected(false);
            }

            if let Some(r) = i.rows.iter().find(|r| r.album_id.as_str() == album_id_str) {
                r.set_selected(true);
                *i.selected_album_id.borrow_mut() = Some(r.album_id.clone());
                i.add_button.set_visible(true);
            }
        });
    }

    {
        let i = Rc::clone(inner);
        add_button.connect_clicked(move |_| {
            let album_id = i.selected_album_id.borrow().clone();
            if let Some(album_id) = album_id {
                debug!(%album_id, count = i.media_ids.len(), "adding to album");
                i.bus_sender.send(AppEvent::AddToAlbumRequested {
                    album_id,
                    ids: i.media_ids.clone(),
                });
                i.dialog.close();
            }
        });
    }

    {
        let i = Rc::clone(inner);
        search_entry.connect_search_changed(move |entry| {
            let query = entry.text().to_string();
            let lower_query = query.to_lowercase();

            for r in &i.rows {
                r.update_search_highlight(&query);
                let matches =
                    lower_query.is_empty() || r.album_name.to_lowercase().contains(&lower_query);
                r.row.set_visible(matches);
            }
        });
    }

    {
        let i = Rc::clone(inner);
        let entry_ref = new_album_entry.clone();
        let do_create = move || {
            let name = entry_ref.text().to_string();
            if name.is_empty() {
                return;
            }
            debug!(%name, count = i.media_ids.len(), "creating album and adding");
            i.bus_sender.send(AppEvent::CreateAlbumRequested {
                name,
                ids: i.media_ids.clone(),
            });
            i.dialog.close();
        };

        let create_fn = Rc::new(do_create);

        {
            let f = Rc::clone(&create_fn);
            create_add_btn.connect_clicked(move |_| f());
        }

        {
            let f = Rc::clone(&create_fn);
            new_album_entry.connect_activate(move |_| f());
        }
    }

    {
        let btn = create_add_btn.clone();
        new_album_entry.connect_changed(move |entry| {
            btn.set_sensitive(!entry.text().is_empty());
        });
    }

    {
        let stack = new_album_stack.clone();
        let key_ctrl = gtk::EventControllerKey::new();
        new_album_entry.add_controller(key_ctrl.clone());
        key_ctrl.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk::gdk::Key::Escape {
                stack.set_visible_child_name("label");
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }

    {
        let d = dialog.clone();
        let i = Rc::clone(inner);
        empty_create_btn.connect_clicked(move |_| {
            let alert = adw::AlertDialog::builder().heading("New Album").build();
            alert.add_response("cancel", "Cancel");
            alert.add_response("create", "Create & add");
            alert.set_response_appearance("create", adw::ResponseAppearance::Suggested);
            alert.set_default_response(Some("create"));
            alert.set_close_response("cancel");

            let entry = gtk::Entry::new();
            entry.set_placeholder_text(Some("Album name"));
            entry.set_activates_default(true);
            alert.set_extra_child(Some(&entry));

            let ids = i.media_ids.clone();
            let tx = i.bus_sender.clone();
            let d2 = d.clone();
            alert.connect_response(None, move |_, response| {
                if response != "create" {
                    return;
                }
                let name = entry.text().to_string();
                if name.is_empty() {
                    return;
                }
                tx.send(AppEvent::CreateAlbumRequested {
                    name,
                    ids: ids.clone(),
                });
                d2.close();
            });

            alert.present(Some(&d));
        });
    }
}
