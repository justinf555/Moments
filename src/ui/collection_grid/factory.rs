use gtk::{glib, prelude::*};

use super::cell::CollectionGridCell;
use super::item::CollectionItemObject;

/// Whether collection thumbnails are displayed as circles or squares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailStyle {
    /// Circular clipping (used for People).
    Circular,
    /// Square with rounded corners (used for Memories, Places, etc.).
    #[allow(dead_code)]
    Square,
}

/// Build a `SignalListItemFactory` for the collection grid.
pub fn build_factory(cell_size: i32, style: ThumbnailStyle) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(move |_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        let cell = CollectionGridCell::new();
        cell.set_size_request(cell_size, cell_size + 48); // Extra height for labels.
        if style == ThumbnailStyle::Circular {
            cell.add_css_class("circular");
        }
        list_item.set_child(Some(&cell));
    });

    factory.connect_bind(|_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        let cell = list_item
            .child()
            .and_downcast::<CollectionGridCell>()
            .expect("child is CollectionGridCell");
        let item = list_item
            .item()
            .and_downcast::<CollectionItemObject>()
            .expect("item is CollectionItemObject");

        cell.bind(&item);

        // Load thumbnail from disk if not yet decoded.
        if item.texture().is_none() {
            if let Some(path) = &item.data().thumbnail_path {
                if path.exists() {
                    let path = path.clone();
                    let item_weak = item.downgrade();
                    glib::MainContext::default().spawn_local(async move {
                        let result = tokio::task::spawn_blocking(move || {
                            gtk::gdk::Texture::from_filename(&path).ok()
                        })
                        .await;
                        if let Ok(Some(texture)) = result {
                            if let Some(item) = item_weak.upgrade() {
                                item.set_texture(Some(&texture));
                            }
                        }
                    });
                }
            }
        }
    });

    factory.connect_unbind(|_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        let cell = list_item
            .child()
            .and_downcast::<CollectionGridCell>()
            .expect("child is CollectionGridCell");
        cell.unbind();

        // Release texture.
        if let Some(item) = list_item
            .item()
            .and_then(|o| o.downcast::<CollectionItemObject>().ok())
        {
            item.set_texture(None::<gtk::gdk::Texture>);
        }
    });

    factory.connect_teardown(|_, obj| {
        let list_item = obj.downcast_ref::<gtk::ListItem>().expect("is ListItem");
        list_item.set_child(None::<&gtk::Widget>);
    });

    factory
}
