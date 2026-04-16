use std::rc::Rc;

use crate::application::MomentsApplication;
use crate::library::album::AlbumId;
use crate::library::media::MediaFilter;
use crate::ui::photo_grid::texture_cache::TextureCache;
use crate::ui::photo_grid::PhotoGridView;

/// Push an album detail photo grid onto the navigation view.
///
/// Used by both item activation (double-click) and the context menu Open action.
pub(crate) fn open_album_drilldown(
    settings: &gtk::gio::Settings,
    texture_cache: &Rc<TextureCache>,
    bus_sender: &crate::event_bus::EventSender,
    nav_view: &adw::NavigationView,
    album_id: AlbumId,
    album_name: &str,
) {
    let filter = MediaFilter::Album { album_id };
    let media_client = MomentsApplication::default()
        .media_client()
        .expect("media client available");
    let store = media_client.create_model(filter.clone());
    let view = PhotoGridView::new();
    view.setup(
        settings.clone(),
        Rc::clone(texture_cache),
        bus_sender.clone(),
    );
    view.set_store(store, filter);

    let page = adw::NavigationPage::builder()
        .tag("album-detail")
        .title(album_name)
        .child(&view)
        .build();

    nav_view.push(&page);
}
