//! Shared album picker popover used by the photo grid action bar and viewers.

use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use tracing::debug;

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::media::MediaId;
use crate::library::Library;

/// Show an album picker popover anchored to a `gtk::Button`.
pub fn show_album_picker(
    btn: &gtk::Button,
    ids: Vec<MediaId>,
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
    bus_sender: EventSender,
) {
    show_album_picker_on_widget(btn.upcast_ref(), ids, library, tokio, bus_sender);
}

/// Show an album picker popover anchored to any widget.
///
/// Lists existing albums (fetched via library query) and a "New Album…"
/// option. All mutations go through the event bus.
pub fn show_album_picker_on_widget(
    widget: &gtk::Widget,
    ids: Vec<MediaId>,
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
    bus_sender: EventSender,
) {
    debug!(count = ids.len(), "album picker: loading albums");

    let widget_weak: glib::WeakRef<gtk::Widget> = widget.downgrade();
    let lib = library;
    let tk = tokio;

    glib::MainContext::default().spawn_local(async move {
        let lib_q = Arc::clone(&lib);
        let albums = match tk.spawn(async move { lib_q.list_albums().await }).await {
            Ok(Ok(a)) => a,
            Ok(Err(e)) => {
                tracing::error!("list_albums failed: {e}");
                return;
            }
            Err(e) => {
                tracing::error!("list_albums join failed: {e}");
                return;
            }
        };

        let Some(parent) = widget_weak.upgrade() else { return };

        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        vbox.set_margin_top(6);
        vbox.set_margin_bottom(6);
        vbox.set_margin_start(6);
        vbox.set_margin_end(6);

        let popover = gtk::Popover::new();
        popover.set_parent(&parent);

        if albums.is_empty() {
            let label = gtk::Label::new(Some("No albums"));
            label.add_css_class("dim-label");
            vbox.append(&label);
        } else {
            for album in &albums {
                let ab = gtk::Button::with_label(&album.name);
                ab.add_css_class("flat");
                let aid = album.id.clone();
                let ids_clone = ids.clone();
                let tx = bus_sender.clone();
                let pop_weak = popover.downgrade();
                ab.connect_clicked(move |_| {
                    if ids_clone.is_empty() {
                        return;
                    }
                    if let Some(p) = pop_weak.upgrade() {
                        p.popdown();
                    }
                    tx.send(AppEvent::AddToAlbumRequested {
                        album_id: aid.clone(),
                        ids: ids_clone.clone(),
                    });
                });
                vbox.append(&ab);
            }
        }

        // Separator + "New Album…" button.
        let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        sep.set_margin_top(4);
        sep.set_margin_bottom(4);
        vbox.append(&sep);

        let new_album_btn = gtk::Button::with_label("New Album\u{2026}");
        new_album_btn.add_css_class("flat");
        {
            let pop_weak = popover.downgrade();
            let ids_clone = ids.clone();
            let tx = bus_sender.clone();
            new_album_btn.connect_clicked(move |btn| {
                // Capture the root window before popdown — the popover's
                // closed handler unparents it, which would make btn.root()
                // return None.
                let window = btn.root().and_downcast::<gtk::Window>();

                if let Some(p) = pop_weak.upgrade() {
                    p.popdown();
                }

                let dialog = adw::AlertDialog::builder()
                    .heading("New Album")
                    .build();
                dialog.add_response("cancel", "Cancel");
                dialog.add_response("create", "Create");
                dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("create"));
                dialog.set_close_response("cancel");

                let entry = gtk::Entry::new();
                entry.set_placeholder_text(Some("Album name"));
                entry.set_activates_default(true);
                dialog.set_extra_child(Some(&entry));

                let ids = ids_clone.clone();
                let tx = tx.clone();
                dialog.connect_response(None, move |_, response| {
                    if response != "create" {
                        return;
                    }
                    let name = entry.text().to_string();
                    if name.is_empty() {
                        return;
                    }
                    tx.send(AppEvent::CreateAlbumRequested { name, ids: ids.clone() });
                });

                dialog.present(window.as_ref());
            });
        }
        vbox.append(&new_album_btn);

        popover.set_child(Some(&vbox));
        popover.connect_closed(move |p| {
            p.unparent();
        });
        popover.popup();
    });
}
