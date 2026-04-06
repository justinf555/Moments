use std::sync::Arc;

use adw::prelude::*;
use adw::subclass::prelude::*;

use crate::app_event::AppEvent;

use super::PhotoViewer;

/// Named references to all buttons in the viewer overflow menu.
pub struct ViewerMenuButtons {
    pub add_to_album: gtk::Button,
    pub share: gtk::Button,
    pub export_original: gtk::Button,
    pub set_wallpaper: Option<gtk::Button>,
    pub show_in_files: gtk::Button,
    pub delete: gtk::Button,
}

/// Build the overflow menu popover content for photo/video viewers.
///
/// `include_wallpaper` controls whether "Set as wallpaper" is shown
/// (photos only, not videos). `delete_label` sets the destructive
/// action label ("Delete photo" vs "Delete video").
///
/// Returns the popover and named button references for direct wiring.
pub fn build_viewer_menu_popover(
    include_wallpaper: bool,
    delete_label: &str,
) -> (gtk::Popover, ViewerMenuButtons) {
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.set_margin_top(6);
    vbox.set_margin_bottom(6);
    vbox.set_margin_start(6);
    vbox.set_margin_end(6);

    // Section 1: actions
    let add_to_album = overflow_btn("Add to album", "folder-new-symbolic");
    let share = overflow_btn("Share", "send-to-symbolic");
    let export_original = overflow_btn("Export original", "document-save-symbolic");
    vbox.append(&add_to_album);
    vbox.append(&share);
    vbox.append(&export_original);

    let set_wallpaper = if include_wallpaper {
        let btn = overflow_btn("Set as wallpaper", "preferences-desktop-wallpaper-symbolic");
        vbox.append(&btn);
        Some(btn)
    } else {
        None
    };

    // Separator
    let sep1 = gtk::Separator::new(gtk::Orientation::Horizontal);
    sep1.set_margin_top(4);
    sep1.set_margin_bottom(4);
    vbox.append(&sep1);

    // Section 2: file system
    let show_in_files = overflow_btn("Show in Files", "folder-open-symbolic");
    vbox.append(&show_in_files);

    // Separator
    let sep2 = gtk::Separator::new(gtk::Orientation::Horizontal);
    sep2.set_margin_top(4);
    sep2.set_margin_bottom(4);
    vbox.append(&sep2);

    // Section 3: destructive
    let delete = overflow_btn(delete_label, "user-trash-symbolic");
    delete.add_css_class("error");
    vbox.append(&delete);

    let popover = gtk::Popover::new();
    popover.set_child(Some(&vbox));

    let buttons = ViewerMenuButtons {
        add_to_album,
        share,
        export_original,
        set_wallpaper,
        show_in_files,
        delete,
    };

    (popover, buttons)
}

/// Create a flat button with icon + label for the overflow menu.
fn overflow_btn(label: &str, icon_name: &str) -> gtk::Button {
    let hbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    hbox.append(&gtk::Image::from_icon_name(icon_name));
    hbox.append(&gtk::Label::new(Some(label)));

    let btn = gtk::Button::builder().child(&hbox).build();
    btn.add_css_class("flat");
    btn
}

/// Wire overflow menu button handlers for the photo viewer.
///
/// Connects: Add to album, Share/Export/Wallpaper/Files stubs, Delete (trash + pop).
pub(super) fn wire_overflow_menu(
    popover: &gtk::Popover,
    buttons: &ViewerMenuButtons,
    viewer: &PhotoViewer,
) {
    // Add to album
    {
        let v = viewer.downgrade();
        let pop = popover.downgrade();
        buttons.add_to_album.connect_clicked(move |_| {
            if let Some(p) = pop.upgrade() {
                p.popdown();
            }
            let Some(viewer) = v.upgrade() else { return };
            let imp = viewer.imp();
            let id = {
                let items = imp.items.borrow();
                let idx = imp.current_index.get();
                items.get(idx).map(|obj| obj.item().id.clone())
            };
            let Some(id) = id else { return };
            crate::ui::album_picker_dialog::show_album_picker_dialog(
                viewer.upcast_ref::<gtk::Widget>(),
                vec![id],
                Arc::clone(imp.library()),
                imp.tokio().clone(),
                imp.bus_sender().clone(),
            );
        });
    }

    // Stub items — just close the popover on click.
    let stubs: Vec<&gtk::Button> = [
        Some(&buttons.share),
        Some(&buttons.export_original),
        buttons.set_wallpaper.as_ref(),
        Some(&buttons.show_in_files),
    ]
    .into_iter()
    .flatten()
    .collect();

    for btn in stubs {
        let pop = popover.downgrade();
        btn.connect_clicked(move |_| {
            if let Some(p) = pop.upgrade() {
                p.popdown();
            }
        });
    }

    // Delete photo — trash + pop back to grid.
    {
        let v = viewer.downgrade();
        let pop = popover.downgrade();
        buttons.delete.connect_clicked(move |_| {
            if let Some(p) = pop.upgrade() {
                p.popdown();
            }
            let Some(viewer) = v.upgrade() else { return };
            let imp = viewer.imp();
            let id = {
                let items = imp.items.borrow();
                let idx = imp.current_index.get();
                items.get(idx).map(|obj| obj.item().id.clone())
            };
            let Some(id) = id else { return };
            imp.bus_sender()
                .send(AppEvent::TrashRequested { ids: vec![id] });
            if let Some(nav_view) = viewer
                .parent()
                .and_then(|p| p.downcast::<adw::NavigationView>().ok())
            {
                nav_view.pop();
            }
        });
    }
}
