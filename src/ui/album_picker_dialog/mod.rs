//! Album picker dialog — lets the user choose or create an album to add
//! selected photos to.

use adw::prelude::*;
use gtk::glib;
use tracing::debug;

use crate::application::MomentsApplication;
use crate::event_bus::EventSender;
use crate::library::media::MediaId;

pub mod album_row;
pub mod dialog;

/// Fetch album data asynchronously and present the picker dialog.
///
/// This is the main entry point — it replaces `album_picker::show_album_picker`.
/// Uses the `AlbumClient` singleton for data loading, then builds and presents
/// the dialog on the GTK main thread.
pub fn show_album_picker_dialog(
    parent: &impl IsA<gtk::Widget>,
    ids: Vec<MediaId>,
    bus_sender: EventSender,
) {
    let album_client = MomentsApplication::default()
        .album_client()
        .expect("album client available");

    let parent_weak: glib::WeakRef<gtk::Widget> = parent.as_ref().downgrade();

    debug!(count = ids.len(), "album picker: loading data");

    album_client.load_picker_data(ids, move |result| {
        let Some(parent) = parent_weak.upgrade() else {
            return;
        };

        match result {
            Ok(data) => {
                debug!(
                    album_count = data.albums.len(),
                    "album picker: presenting dialog"
                );
                dialog::present(data, bus_sender, &parent);
            }
            Err(e) => {
                tracing::error!("album picker data load failed: {e}");
                bus_sender.send(crate::app_event::AppEvent::Error(
                    "Could not load albums".into(),
                ));
            }
        }
    });
}
