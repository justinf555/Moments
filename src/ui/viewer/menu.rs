use std::sync::Arc;

use adw::prelude::*;
use adw::subclass::prelude::*;

use crate::app_event::AppEvent;

use super::PhotoViewer;

/// Build the overflow menu popover content for photo/video viewers.
///
/// `include_wallpaper` controls whether "Set as wallpaper" is shown
/// (photos only, not videos). `delete_label` sets the destructive
/// action label ("Delete photo" vs "Delete video").
pub fn build_viewer_menu_popover(include_wallpaper: bool, delete_label: &str) -> gtk::Popover {
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.set_margin_top(6);
    vbox.set_margin_bottom(6);
    vbox.set_margin_start(6);
    vbox.set_margin_end(6);

    // Section 1: actions
    vbox.append(&overflow_btn("Add to album", "folder-new-symbolic", "add-to-album"));
    vbox.append(&overflow_btn("Share", "send-to-symbolic", "share"));
    vbox.append(&overflow_btn("Export original", "document-save-symbolic", "export-original"));
    if include_wallpaper {
        vbox.append(&overflow_btn(
            "Set as wallpaper",
            "preferences-desktop-wallpaper-symbolic",
            "set-wallpaper",
        ));
    }

    // Separator
    let sep1 = gtk::Separator::new(gtk::Orientation::Horizontal);
    sep1.set_margin_top(4);
    sep1.set_margin_bottom(4);
    vbox.append(&sep1);

    // Section 2: file system
    vbox.append(&overflow_btn(
        "Show in Files",
        "folder-open-symbolic",
        "show-in-files",
    ));

    // Separator
    let sep2 = gtk::Separator::new(gtk::Orientation::Horizontal);
    sep2.set_margin_top(4);
    sep2.set_margin_bottom(4);
    vbox.append(&sep2);

    // Section 3: destructive
    let delete_btn = overflow_btn(delete_label, "user-trash-symbolic", "delete");
    delete_btn.add_css_class("error");
    vbox.append(&delete_btn);

    let popover = gtk::Popover::new();
    popover.set_child(Some(&vbox));
    popover
}

/// Create a flat button with icon + label for the overflow menu.
fn overflow_btn(label: &str, icon_name: &str, widget_name: &str) -> gtk::Button {
    let hbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    hbox.append(&gtk::Image::from_icon_name(icon_name));
    hbox.append(&gtk::Label::new(Some(label)));

    let btn = gtk::Button::builder().child(&hbox).build();
    btn.add_css_class("flat");
    btn.set_widget_name(widget_name);
    btn
}

/// Find a button in the popover by its widget name.
pub fn find_menu_button(popover: &gtk::Popover, name: &str) -> Option<gtk::Button> {
    let child = popover.child()?;
    let vbox = child.downcast_ref::<gtk::Box>()?;
    let mut widget = vbox.first_child();
    while let Some(w) = widget {
        if let Some(btn) = w.downcast_ref::<gtk::Button>() {
            if btn.widget_name() == name {
                return Some(btn.clone());
            }
        }
        widget = w.next_sibling();
    }
    None
}

/// Wire overflow menu button handlers for the photo viewer.
///
/// Connects: Add to album, Share/Export/Wallpaper/Files stubs, Delete (trash + pop).
pub(super) fn wire_overflow_menu(
    popover: &gtk::Popover,
    viewer: &PhotoViewer,
) {
    // Add to album
    if let Some(btn) = find_menu_button(popover, "add-to-album") {
        let v = viewer.downgrade();
        let pop = popover.downgrade();
        btn.connect_clicked(move |_| {
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
                Arc::clone(imp.library.get().unwrap()),
                imp.tokio.get().unwrap().clone(),
                imp.bus_sender.get().unwrap().clone(),
            );
        });
    }

    // Stub items — just close the popover on click.
    for name in &["share", "export-original", "set-wallpaper", "show-in-files"] {
        if let Some(btn) = find_menu_button(popover, name) {
            let pop = popover.downgrade();
            btn.connect_clicked(move |_| {
                if let Some(p) = pop.upgrade() {
                    p.popdown();
                }
            });
        }
    }

    // Delete photo — trash + pop back to grid.
    if let Some(btn) = find_menu_button(popover, "delete") {
        let v = viewer.downgrade();
        let pop = popover.downgrade();
        btn.connect_clicked(move |_| {
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
            imp.bus_sender.get().unwrap().send(AppEvent::TrashRequested { ids: vec![id] });
            if let Some(nav_view) = viewer
                .parent()
                .and_then(|p| p.downcast::<adw::NavigationView>().ok())
            {
                nav_view.pop();
            }
        });
    }
}
