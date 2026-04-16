//! Album picker dialog — lets the user choose or create an album to add
//! selected photos to.

use adw::prelude::*;
use gtk::glib;
use tracing::debug;

use crate::application::MomentsApplication;
use crate::library::media::MediaId;

pub mod album_row;
pub mod dialog;

/// Fetch album membership data and present the picker dialog.
///
/// Uses `AlbumClientV2` for the membership query and album commands.
/// Album list comes from a shared model; thumbnails are handled by the client.
pub fn show_album_picker_dialog(parent: &impl IsA<gtk::Widget>, ids: Vec<MediaId>) {
    let album_client = MomentsApplication::default()
        .album_client_v2()
        .expect("album client v2 available");

    let parent_weak: glib::WeakRef<gtk::Widget> = parent.as_ref().downgrade();

    debug!(count = ids.len(), "album picker: loading membership data");

    let ac = album_client.clone();
    let media_ids = ids.clone();
    glib::MainContext::default().spawn_local(async move {
        let membership = ac.album_membership(ids).await;

        let Some(parent) = parent_weak.upgrade() else {
            return;
        };

        match membership {
            Ok(membership) => {
                debug!("album picker: presenting dialog");
                dialog::present(album_client, media_ids, membership, &parent);
            }
            Err(e) => {
                tracing::error!("album picker membership load failed: {e}");
                crate::client::show_error_toast(&e);
            }
        }
    });
}
