use std::cell::RefCell;
use std::rc::Rc;

use crate::library::album::AlbumId;
use crate::library::media::{MediaFilter, MediaId, MediaItem};
use crate::ui::photo_grid::PhotoGridModel;

/// Shared registry of all active [`PhotoGridModel`] instances.
///
/// The application's idle loop calls [`on_thumbnail_ready`](Self::on_thumbnail_ready)
/// and [`reload_all`](Self::reload_all) to broadcast library events. Models
/// register themselves at creation time — either during startup (eager) or on
/// first navigation (lazy).
///
/// See `docs/design-lazy-view-loading.md` for the full design rationale.
pub struct ModelRegistry {
    models: RefCell<Vec<Rc<PhotoGridModel>>>,
}

impl ModelRegistry {
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            models: RefCell::new(Vec::new()),
        })
    }

    /// Add a model to the registry. Called when a view is created.
    pub fn register(&self, model: &Rc<PhotoGridModel>) {
        self.models.borrow_mut().push(Rc::clone(model));
    }

    /// Forward a `ThumbnailReady` event to all registered models.
    pub fn on_thumbnail_ready(&self, id: &MediaId) {
        for model in self.models.borrow().iter() {
            model.on_thumbnail_ready(id);
        }
    }

    /// Broadcast a favourite change to all registered models.
    ///
    /// Unfiltered models update the item's property in place. Filtered
    /// models (Favorites) reload from the database to add/remove the item.
    pub fn on_favorite_changed(&self, id: &MediaId, is_favorite: bool) {
        for model in self.models.borrow().iter() {
            model.on_favorite_changed(id, is_favorite);
        }
    }

    /// Broadcast a trash/restore change to all registered models.
    pub fn on_trashed(&self, id: &MediaId, is_trashed: bool) {
        for model in self.models.borrow().iter() {
            model.on_trashed(id, is_trashed);
        }
    }

    /// Broadcast a permanent deletion to all registered models.
    /// Removes the item from all views — no reload.
    pub fn on_deleted(&self, id: &MediaId) {
        for model in self.models.borrow().iter() {
            model.on_deleted(id);
        }
    }

    /// Reload the model for a specific album (e.g. after add/remove).
    pub fn on_album_media_changed(&self, album_id: &AlbumId) {
        for model in self.models.borrow().iter() {
            if let MediaFilter::Album { album_id: ref mid } = model.filter() {
                if mid == album_id {
                    model.reload();
                }
            }
        }
    }

    /// Insert a synced asset into all matching model views.
    ///
    /// Each model checks its filter — if the item matches, it's inserted
    /// at the correct sorted position without clearing the store.
    pub fn on_asset_synced(&self, item: &MediaItem) {
        for model in self.models.borrow().iter() {
            let filter = model.filter();
            if filter.matches(item) {
                model.insert_item_sorted(item.clone());
            }
        }
    }

    /// Reload all registered models (e.g. after import completes).
    pub fn reload_all(&self) {
        for model in self.models.borrow().iter() {
            model.reload();
        }
    }
}

impl std::fmt::Debug for ModelRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelRegistry")
            .field("count", &self.models.borrow().len())
            .finish()
    }
}
