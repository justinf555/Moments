use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib};
use tracing::{debug, error};

use crate::app_event::AppEvent;

use super::PhotoViewer;

impl PhotoViewer {
    /// Asynchronously load the original file at full resolution.
    pub(super) fn start_full_res_load(&self, gen: u64, id: crate::library::media::MediaId) {
        let imp = self.imp();
        let bus_sender = imp.bus_sender().clone();
        let tokio = crate::application::MomentsApplication::default().tokio_handle();

        imp.spinner.set_spinning(true);
        imp.spinner.set_visible(true);

        let media_client = crate::application::MomentsApplication::default()
            .media_client()
            .expect("media client available");

        let weak = self.downgrade();
        media_client.original_path(&id, move |path| {
            let Some(path) = path else {
                if let Some(viewer) = weak.upgrade() {
                    let imp = viewer.imp();
                    imp.spinner.set_spinning(false);
                    imp.spinner.set_visible(false);
                }
                bus_sender.send(AppEvent::Error("Could not find original photo".into()));
                return;
            };

            let Some(viewer) = weak.upgrade() else { return };
            let imp = viewer.imp();

            if imp.load_gen.get() != gen {
                imp.spinner.set_spinning(false);
                imp.spinner.set_visible(false);
                return;
            }

            // Guard: skip decode for video files (detect by magic bytes).
            let is_video = {
                use crate::library::format::detect::{detect_format, DetectedFormat};
                matches!(detect_format(&path), Ok(DetectedFormat::Video(_)))
            };
            if is_video {
                imp.spinner.set_spinning(false);
                imp.spinner.set_visible(false);
                return;
            }

            // Detect RAW files by trying magic bytes — TIFF header that isn't
            // a standard TIFF often indicates a RAW format (CR2, DNG, NEF, etc.).
            // The RAW handler will confirm during decode.
            let is_raw = {
                use crate::library::format::detect::{detect_format, DetectedFormat, ImageFormat};
                // If magic bytes say Unknown, it's likely a RAW format that
                // rawler can handle. If TIFF, it could be either — try RAW first.
                match detect_format(&path) {
                    Ok(DetectedFormat::Image(ImageFormat::Tiff)) => true,
                    Ok(DetectedFormat::Unknown) => true,
                    _ => false,
                }
            };
            let weak2 = viewer.downgrade();
            drop(viewer);
            glib::MainContext::default().spawn_local(async move {
                let pixels: Option<(Vec<u8>, i32, i32)> = tokio
                    .spawn(async move {
                        tokio::task::spawn_blocking(move || -> Option<(Vec<u8>, i32, i32)> {
                            let img = if is_raw {
                                use crate::library::format::raw::RawHandler;
                                // Try RAW decoder first; fall back to standard.
                                RawHandler
                                    .decode_full_res(&path)
                                    .or_else(|_| {
                                        image::ImageReader::open(&path)
                                            .and_then(|r| r.with_guessed_format())
                                            .map_err(|e| crate::library::error::LibraryError::Thumbnail(e.to_string()))
                                            .and_then(|r| r.decode().map_err(|e| crate::library::error::LibraryError::Thumbnail(e.to_string())))
                                    })
                                    .map_err(|e| debug!("full-res decode failed: {e}"))
                                    .ok()?
                            } else {
                                image::ImageReader::open(&path)
                                    .and_then(|r| r.with_guessed_format())
                                    .ok()
                                    .and_then(|r| r.decode().ok())
                                    .or_else(|| {
                                        debug!("full-res decode failed");
                                        None
                                    })?
                            };
                            let is_heif = {
                                use crate::library::format::detect::{detect_format, DetectedFormat, ImageFormat};
                                matches!(detect_format(&path), Ok(DetectedFormat::Image(ImageFormat::Heif)))
                            };
                            let img = if is_heif || is_raw {
                                img
                            } else {
                                let orientation =
                                    crate::library::metadata::exif::extract_exif(&path)
                                        .orientation
                                        .unwrap_or(1);
                                crate::library::thumbnail::thumbnailer::apply_orientation(
                                    img,
                                    orientation,
                                )
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

                let Some(viewer) = weak2.upgrade() else {
                    return;
                };
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
                        imp.bus_sender().send(AppEvent::Error(
                            "Could not display full-resolution image".into(),
                        ));
                    }
                }
            });
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
        let tokio = crate::application::MomentsApplication::default().tokio_handle();

        let media_client = crate::application::MomentsApplication::default()
            .media_client()
            .expect("media client available");

        // Fetch edit state and original path in parallel via two client calls.
        let weak = self.downgrade();
        let id_for_state = id.clone();

        // We need both results. Use shared state to collect them.
        let state_slot: std::rc::Rc<
            std::cell::RefCell<Option<Option<crate::library::editing::EditState>>>,
        > = std::rc::Rc::new(std::cell::RefCell::new(None));
        let path_slot: std::rc::Rc<std::cell::RefCell<Option<Option<std::path::PathBuf>>>> =
            std::rc::Rc::new(std::cell::RefCell::new(None));

        let try_start = {
            let state_slot = state_slot.clone();
            let path_slot = path_slot.clone();
            let weak = weak.clone();
            let id = id.clone();
            let tokio = tokio.clone();
            move || {
                let state_ready = state_slot.borrow().is_some();
                let path_ready = path_slot.borrow().is_some();
                if !state_ready || !path_ready {
                    return;
                }

                let existing_state = state_slot.borrow().as_ref().unwrap().clone();
                let path = path_slot.borrow().as_ref().unwrap().clone();

                let Some(path) = path else {
                    error!("could not resolve original path for edit session");
                    return;
                };

                let weak = weak.clone();
                let tk = tokio.clone();
                glib::MainContext::default().spawn_local(async move {
                    let preview = tk
                        .spawn(async move {
                            tokio::task::spawn_blocking(
                                move || -> Option<std::sync::Arc<image::DynamicImage>> {
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
                                            .map_err(|e| {
                                                error!("edit session RAW decode failed: {e}")
                                            })
                                            .ok()?
                                    } else {
                                        image::open(&path)
                                            .map_err(|e| error!("edit session decode failed: {e}"))
                                            .ok()?
                                    };
                                    let img = if matches!(ext.as_str(), "heic" | "heif") || is_raw {
                                        img
                                    } else {
                                        let orientation =
                                            crate::library::metadata::exif::extract_exif(&path)
                                                .orientation
                                                .unwrap_or(1);
                                        crate::library::thumbnail::thumbnailer::apply_orientation(
                                            img,
                                            orientation,
                                        )
                                    };
                                    let (w, h) = image::GenericImageView::dimensions(&img);
                                    let preview = if w <= 1200 && h <= 1200 {
                                        img
                                    } else {
                                        img.thumbnail(1200, 1200)
                                    };
                                    Some(std::sync::Arc::new(preview))
                                },
                            )
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

                    if imp.load_gen.get() != gen {
                        return;
                    }

                    let panel = imp.edit_panel.borrow();
                    if let Some(ref panel) = *panel {
                        panel.begin_session(id, preview, existing_state);
                    }
                });
            }
        };

        // Fire both queries. Each callback stores its result and tries to
        // start the session. The second one to arrive will have both slots
        // filled and proceed.
        {
            let try_start = try_start.clone();
            media_client.get_edit_state(&id_for_state, move |state| {
                *state_slot.borrow_mut() = Some(state);
                try_start();
            });
        }
        {
            media_client.original_path(&id, move |path| {
                *path_slot.borrow_mut() = Some(path);
                try_start();
            });
        }
    }

    /// Asynchronously fetch EXIF metadata and cache it for the info panel.
    pub(super) fn load_metadata_async(&self, gen: u64, id: crate::library::media::MediaId) {
        let media_client = crate::application::MomentsApplication::default()
            .media_client()
            .expect("media client available");

        let weak = self.downgrade();
        media_client.media_metadata(&id, move |metadata| {
            let Some(viewer) = weak.upgrade() else { return };
            let imp = viewer.imp();

            if imp.load_gen.get() != gen {
                return;
            }

            *imp.current_metadata.borrow_mut() = metadata;

            if imp.info_split.shows_sidebar() {
                let items = imp.items.borrow();
                let idx = imp.current_index.get();
                if let Some(obj) = items.get(idx) {
                    let item = obj.item().clone();
                    let meta = imp.current_metadata.borrow();
                    if let Some(ref panel) = *imp.info_panel.borrow() {
                        panel.set_item(&item, meta.as_ref());
                    }
                }
            }
        });
    }
}
