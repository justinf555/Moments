//! Album picker dialog — lets the user choose or create an album to add
//! selected photos to.

use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use tracing::debug;

use crate::event_bus::EventSender;
use crate::library::error::LibraryError;
use crate::library::media::MediaId;
use crate::library::Library;

pub mod album_row;
pub mod dialog;
pub mod state;

pub use state::{AlbumEntry, AlbumPickerData};

/// Fetch album data asynchronously and present the picker dialog.
///
/// This is the main entry point — it replaces `album_picker::show_album_picker`.
/// Spawns a task to load albums + membership data, then builds and presents
/// the dialog on the GTK main thread.
pub fn show_album_picker_dialog(
    parent: &impl IsA<gtk::Widget>,
    ids: Vec<MediaId>,
    library: Arc<Library>,
    tokio: tokio::runtime::Handle,
    bus_sender: EventSender,
) {
    let parent_weak: glib::WeakRef<gtk::Widget> = parent.as_ref().downgrade();
    let lib = library;
    let tk = tokio;

    debug!(count = ids.len(), "album picker: loading data");

    glib::MainContext::default().spawn_local(async move {
        let lib_q = Arc::clone(&lib);
        let ids_q = ids.clone();

        let result = tk
            .spawn(async move {
                let albums = lib_q.list_albums().await?;
                let containing = lib_q.albums_containing_media(&ids_q).await?;
                Ok::<_, LibraryError>((albums, containing))
            })
            .await;

        let (albums, containing) = match result {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                tracing::error!("album picker data load failed: {e}");
                bus_sender.send(crate::app_event::AppEvent::Error(
                    "Could not load albums".into(),
                ));
                return;
            }
            Err(e) => {
                tracing::error!("album picker join failed: {e}");
                bus_sender.send(crate::app_event::AppEvent::Error(
                    "Could not load albums".into(),
                ));
                return;
            }
        };

        // Resolve thumbnail paths and pre-decode on Tokio to avoid
        // blocking the GTK thread with synchronous file I/O.
        let thumb_entries: Vec<_> = albums
            .iter()
            .map(|a| {
                let path = a.cover_media_id.as_ref().map(|mid| lib.thumbnail_path(mid));
                (a.id.clone(), path)
            })
            .collect();

        let decoded = tk
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    thumb_entries
                        .into_iter()
                        .map(|(id, path)| {
                            let rgba = path.and_then(|p| {
                                let data = std::fs::read(&p).ok()?;
                                let img = image::load_from_memory(&data).ok()?;
                                let rgba = img.to_rgba8();
                                let (w, h) = image::GenericImageView::dimensions(&rgba);
                                Some((rgba.into_raw(), w, h))
                            });
                            (id, rgba)
                        })
                        .collect::<Vec<_>>()
                })
                .await
                .unwrap_or_default()
            })
            .await
            .unwrap_or_default();

        let decoded_map: std::collections::HashMap<_, _> = decoded.into_iter().collect();

        let Some(parent) = parent_weak.upgrade() else {
            return;
        };

        let entries: Vec<AlbumEntry> = albums
            .into_iter()
            .map(|a| {
                let already = containing.get(&a.id).copied().unwrap_or(0);
                let thumbnail_rgba = decoded_map.get(&a.id).and_then(|opt| opt.clone());
                AlbumEntry {
                    id: a.id,
                    name: a.name,
                    media_count: a.media_count,
                    thumbnail_rgba,
                    already_added_count: already,
                }
            })
            .collect();

        debug!(
            album_count = entries.len(),
            "album picker: presenting dialog"
        );

        let data = AlbumPickerData {
            albums: entries,
            media_ids: ids,
        };
        dialog::present(data, bus_sender, &parent);
    });
}
