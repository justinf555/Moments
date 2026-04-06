use std::sync::Arc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib};
use tracing::{debug, error};

use crate::app_event::AppEvent;

use super::PhotoViewer;

impl PhotoViewer {
    /// Asynchronously load the original file at full resolution.
    ///
    /// Strategy:
    /// 1. Resolve the original path from the library.
    /// 2. Decode via `image::open()` on a blocking thread and upload RGBA
    ///    bytes as a `gdk::MemoryTexture`.
    /// 3. EXIF orientation is always applied before display.
    ///
    /// Falls back silently to the cached thumbnail on any error.
    pub(super) fn start_full_res_load(&self, gen: u64, id: crate::library::media::MediaId) {
        let imp = self.imp();
        let library = Arc::clone(imp.library.get().unwrap());
        let tokio = imp.tokio.get().unwrap().clone();
        let bus_sender = imp.bus_sender.get().unwrap().clone();

        imp.spinner.set_spinning(true);
        imp.spinner.set_visible(true);

        let weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            // Resolve path on Tokio (async DB call).
            let path = match tokio
                .spawn(async move { library.original_path(&id).await })
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
            {
                Some(p) => p,
                None => {
                    if let Some(viewer) = weak.upgrade() {
                        let imp = viewer.imp();
                        imp.spinner.set_spinning(false);
                        imp.spinner.set_visible(false);
                    }
                    bus_sender.send(AppEvent::Error("Could not find original photo".into()));
                    return;
                }
            };

            let Some(viewer) = weak.upgrade() else { return };
            let imp = viewer.imp();

            if imp.load_gen.get() != gen {
                imp.spinner.set_spinning(false);
                imp.spinner.set_visible(false);
                return;
            }

            // Guard: skip decode for video files (they use VideoViewer).
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase())
                .unwrap_or_default();
            if crate::library::format::registry::VIDEO_EXTENSIONS.contains(&ext.as_str()) {
                imp.spinner.set_spinning(false);
                imp.spinner.set_visible(false);
                return;
            }

            // Decode full-res image on a blocking thread.
            // RAW formats use rawler; standard formats use the image crate.
            let is_raw = crate::library::format::registry::RAW_EXTENSIONS.contains(&ext.as_str());
            let tokio = imp.tokio.get().unwrap().clone();
            drop(viewer); // Release ref before long-running decode.
            let pixels: Option<(Vec<u8>, i32, i32)> = tokio
                .spawn(async move {
                    tokio::task::spawn_blocking(move || -> Option<(Vec<u8>, i32, i32)> {
                        let img = if is_raw {
                            use crate::library::format::raw::RawHandler;
                            RawHandler
                                .decode_full_res(&path)
                                .map_err(|e| debug!("RAW full-res decode failed: {e}"))
                                .ok()?
                        } else {
                            image::open(&path)
                                .map_err(|e| debug!("full-res decode failed: {e}"))
                                .ok()?
                        };
                        // Skip orientation for HEIC/HEIF (libheif applies it
                        // automatically) and RAW (embedded JPEG previews from
                        // cameras are typically pre-rotated; full demosaic
                        // output from rawler is also pre-oriented). Applying
                        // EXIF orientation again would double-rotate.
                        let ext = path
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.to_lowercase())
                            .unwrap_or_default();
                        let img = if matches!(ext.as_str(), "heic" | "heif") || is_raw {
                            img
                        } else {
                            let orientation = crate::library::exif::extract_exif(&path)
                                .orientation
                                .unwrap_or(1);
                            crate::library::thumbnailer::apply_orientation(img, orientation)
                        };
                        let rgba = img.into_rgba8();
                        let (w, h) = rgba.dimensions();
                        Some((rgba.into_raw(), w as i32, h as i32))
                    })
                    .await
                    .ok()?
                })
                .await
                .ok()
                .flatten();

            let Some(viewer) = weak.upgrade() else { return };
            let imp = viewer.imp();

            imp.spinner.set_spinning(false);
            imp.spinner.set_visible(false);

            if imp.load_gen.get() != gen {
                return;
            }

            match pixels {
                Some((raw, width, height)) => {
                    let gbytes = glib::Bytes::from_owned(raw);
                    let texture = gdk::MemoryTexture::new(
                        width,
                        height,
                        gdk::MemoryFormat::R8g8b8a8,
                        &gbytes,
                        (width as usize) * 4,
                    )
                    .upcast::<gdk::Texture>();
                    imp.picture
                        .set_paintable(Some(texture.upcast_ref::<gdk::Paintable>()));
                    debug!("full-res via MemoryTexture: {width}×{height}");
                }
                None => {
                    debug!("full-res decode failed, keeping thumbnail");
                    imp.bus_sender.get().unwrap().send(AppEvent::Error(
                        "Could not display full-resolution image".into(),
                    ));
                }
            }
        });
    }

    /// Start an edit session by loading the full-res image and existing edit state.
    pub(super) fn start_edit_session(&self) {
        let imp = self.imp();
        let id = {
            let items = imp.items.borrow();
            let idx = imp.current_index.get();
            items.get(idx).map(|obj| obj.item().id.clone())
        };
        let Some(id) = id else { return };

        let gen = imp.load_gen.get();
        let library = Arc::clone(imp.library.get().unwrap());
        let tokio = imp.tokio.get().unwrap().clone();
        let id_for_state = id.clone();

        let weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            // Load existing edit state and the original image in parallel.
            let lib = Arc::clone(&library);
            let tk = tokio.clone();

            let state_handle = tk.spawn({
                let lib = Arc::clone(&lib);
                let id = id_for_state.clone();
                async move { lib.get_edit_state(&id).await }
            });
            let path_handle = tk.spawn({
                let lib = Arc::clone(&lib);
                let id = id.clone();
                async move { lib.original_path(&id).await }
            });
            let (state_result, path_result) = tokio::join!(state_handle, path_handle);

            let path = path_result.ok().and_then(|r| r.ok()).flatten();

            let Some(path) = path else {
                error!("could not resolve original path for edit session");
                return;
            };

            // Decode the full-res image and create a downscaled preview on
            // a blocking thread so the GTK thread is free for the sidebar
            // slide-in animation.
            let preview = tk
                .spawn(async move {
                    tokio::task::spawn_blocking(move || -> Option<Arc<image::DynamicImage>> {
                        let ext = path
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.to_lowercase())
                            .unwrap_or_default();
                        let is_raw = crate::library::format::registry::RAW_EXTENSIONS
                            .contains(&ext.as_str());
                        let img = if is_raw {
                            use crate::library::format::raw::RawHandler;
                            RawHandler
                                .decode_full_res(&path)
                                .map_err(|e| error!("edit session RAW decode failed: {e}"))
                                .ok()?
                        } else {
                            image::open(&path)
                                .map_err(|e| error!("edit session decode failed: {e}"))
                                .ok()?
                        };
                        // Skip EXIF orientation for HEIC (libheif pre-applies)
                        // and RAW (embedded previews / demosaic output are
                        // pre-oriented).
                        let img = if matches!(ext.as_str(), "heic" | "heif") || is_raw {
                            img
                        } else {
                            let orientation = crate::library::exif::extract_exif(&path)
                                .orientation
                                .unwrap_or(1);
                            crate::library::thumbnailer::apply_orientation(img, orientation)
                        };
                        // Downscale to ~1200px for fast preview rendering.
                        let (w, h) = image::GenericImageView::dimensions(&img);
                        let preview = if w <= 1200 && h <= 1200 {
                            img
                        } else {
                            img.thumbnail(1200, 1200)
                        };
                        Some(Arc::new(preview))
                    })
                    .await
                    .ok()?
                })
                .await
                .ok()
                .flatten();

            let Some(preview) = preview else {
                error!("failed to decode image for edit session");
                return;
            };

            let Some(viewer) = weak.upgrade() else { return };
            let imp = viewer.imp();

            // Discard stale result if the user navigated away during decode.
            if imp.load_gen.get() != gen {
                return;
            }

            let existing_state = state_result.ok().and_then(|r| r.ok()).flatten();

            let panel = imp.edit_panel.borrow();
            if let Some(ref panel) = *panel {
                panel.begin_session(id, preview, existing_state);
            }
            drop(panel);
        });
    }

    /// Asynchronously fetch EXIF metadata and cache it for the info panel.
    pub(super) fn load_metadata_async(&self, gen: u64, id: crate::library::media::MediaId) {
        let imp = self.imp();
        let library = Arc::clone(imp.library.get().unwrap());
        let tokio = imp.tokio.get().unwrap().clone();

        let weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let metadata = tokio
                .spawn(async move { library.media_metadata(&id).await })
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten();

            let Some(viewer) = weak.upgrade() else { return };
            let imp = viewer.imp();

            if imp.load_gen.get() != gen {
                return; // stale
            }

            *imp.current_metadata.borrow_mut() = metadata;

            // If the panel is open, refresh it with the newly arrived metadata.
            if imp.info_split.shows_sidebar() {
                let items = imp.items.borrow();
                let idx = imp.current_index.get();
                if let Some(obj) = items.get(idx) {
                    let item = obj.item().clone();
                    let meta = imp.current_metadata.borrow();
                    if let Some(ref panel) = *imp.info_panel.borrow() {
                        panel.populate(&item, meta.as_ref());
                    }
                }
            }
        });
    }
}
