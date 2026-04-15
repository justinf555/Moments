use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib};
use tracing::{debug, error};

use crate::app_event::AppEvent;
use crate::renderer::output;
use crate::renderer::pipeline::{RenderOptions, RenderSize};

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

        let pipeline = match crate::application::MomentsApplication::default().render_pipeline() {
            Some(p) => p,
            None => {
                error!("render pipeline not available");
                imp.spinner.set_spinning(false);
                imp.spinner.set_visible(false);
                return;
            }
        };

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
                use crate::renderer::format::detect::{detect_format, DetectedFormat};
                matches!(detect_format(&path), Ok(DetectedFormat::Video(_)))
            };
            if is_video {
                imp.spinner.set_spinning(false);
                imp.spinner.set_visible(false);
                return;
            }

            let weak2 = viewer.downgrade();
            let bus = imp.bus_sender().clone();
            drop(viewer);
            glib::MainContext::default().spawn_local(async move {
                let pixels: Option<(Vec<u8>, u32, u32)> = tokio
                    .spawn(async move {
                        tokio::task::spawn_blocking(move || {
                            let options = RenderOptions {
                                size: RenderSize::FullRes,
                                edits: None,
                            };
                            match pipeline.render(&path, &options) {
                                Ok(img) => Some(output::to_rgba(&img)),
                                Err(e) => {
                                    debug!("full-res decode failed: {e}");
                                    None
                                }
                            }
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
                            width as i32,
                            height as i32,
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
                        bus.send(AppEvent::Error(
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

        let pipeline = match crate::application::MomentsApplication::default().render_pipeline() {
            Some(p) => p,
            None => {
                error!("render pipeline not available for edit session");
                return;
            }
        };

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
                let pipeline = pipeline.clone();
                glib::MainContext::default().spawn_local(async move {
                    let preview = tk
                        .spawn(async move {
                            tokio::task::spawn_blocking(
                                move || -> Option<std::sync::Arc<image::DynamicImage>> {
                                    let options = RenderOptions {
                                        size: RenderSize::Thumbnail(1200),
                                        edits: None,
                                    };
                                    let img = pipeline
                                        .render(&path, &options)
                                        .map_err(|e| error!("edit session decode failed: {e}"))
                                        .ok()?;
                                    Some(std::sync::Arc::new(img))
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
