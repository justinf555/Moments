use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::gio;

use crate::application::MomentsApplication;
use crate::library::album::AlbumId;

use super::card;
use crate::client::AlbumItemObject;

/// Configuration for wiring selection mode on the album grid.
pub(crate) struct SelectionConfig<'a> {
    pub enter_selection: &'a gio::SimpleAction,
    pub header: &'a adw::HeaderBar,
    pub new_album_btn: &'a gtk::Button,
    pub menu_btn: &'a gtk::MenuButton,
    pub cancel_btn: &'a gtk::Button,
    pub action_bar: &'a gtk::ActionBar,
    pub grid_view: &'a gtk::GridView,
    pub multi_selection: &'a gtk::MultiSelection,
    pub store: &'a gio::ListStore,
    pub selection_mode: &'a Rc<std::cell::Cell<bool>>,
}

/// Wire up selection mode UI transitions and batch-delete button.
///
/// `enter_selection` is created by the caller (needed by the factory before
/// the grid view exists). This function wires its activate handler and
/// creates the `exit_selection` action.
pub(crate) fn wire_selection_mode(cfg: &SelectionConfig<'_>) {
    let selection_title = gtk::Label::new(Some("0 selected"));
    selection_title.add_css_class("heading");

    // ── Enter selection mode ────────────────────────────────────────
    {
        let sm = Rc::clone(cfg.selection_mode);
        let new_btn = cfg.new_album_btn.clone();
        let menu = cfg.menu_btn.clone();
        let cancel = cfg.cancel_btn.clone();
        let title = selection_title.clone();
        let bar = cfg.action_bar.clone();
        let gv = cfg.grid_view.clone();
        let hdr = cfg.header.clone();
        cfg.enter_selection.connect_activate(move |_, _| {
            sm.set(true);
            new_btn.set_visible(false);
            menu.set_visible(false);
            cancel.set_visible(true);
            title.set_text("0 selected");
            hdr.set_title_widget(Some(&title));
            bar.set_revealed(true);
            set_cards_selection_mode(&gv, true);
        });
    }

    // ── Exit selection mode ─────────────────────────────────────────
    let exit_selection = gio::SimpleAction::new("exit-selection", None);
    {
        let sm = Rc::clone(cfg.selection_mode);
        let new_btn = cfg.new_album_btn.clone();
        let menu = cfg.menu_btn.clone();
        let cancel = cfg.cancel_btn.clone();
        let bar = cfg.action_bar.clone();
        let gv = cfg.grid_view.clone();
        let sel = cfg.multi_selection.clone();
        let hdr = cfg.header.clone();
        exit_selection.connect_activate(move |_, _| {
            sm.set(false);
            new_btn.set_visible(true);
            menu.set_visible(true);
            cancel.set_visible(false);
            hdr.set_title_widget(None::<&gtk::Widget>);
            bar.set_revealed(false);
            sel.unselect_all();
            set_cards_selection_mode(&gv, false);
        });
    }

    // Cancel button → exit selection.
    {
        let exit = exit_selection.clone();
        cfg.cancel_btn.connect_clicked(move |_| {
            exit.activate(None);
        });
    }

    // Update selection count label when selection changes.
    {
        let title = selection_title.clone();
        cfg.multi_selection.connect_selection_changed(move |sel, _, _| {
            let count = sel.selection().size() as u32;
            title.set_text(&format!("{count} selected"));
        });
    }

    // Wire batch-delete button in the action bar.
    wire_batch_delete(
        cfg.action_bar,
        cfg.multi_selection,
        cfg.store,
        &exit_selection,
    );
}

/// Walk the grid view's children and toggle selection checkboxes on all cards.
fn set_cards_selection_mode(grid_view: &gtk::GridView, mode: bool) {
    let mut child = grid_view.first_child();
    while let Some(c) = child {
        if let Some(card_widget) = c
            .first_child()
            .and_then(|w| w.downcast::<card::AlbumCard>().ok())
        {
            card_widget.set_selection_mode(mode);
        }
        child = c.next_sibling();
    }
}

/// Wire the "Delete Albums" action bar button to show a confirmation dialog
/// and delete all selected albums via `AlbumClientV2`.
fn wire_batch_delete(
    action_bar: &gtk::ActionBar,
    multi_selection: &gtk::MultiSelection,
    store: &gio::ListStore,
    exit_selection: &gio::SimpleAction,
) {
    let delete_btn = gtk::Button::with_label(&gettext("Delete Albums"));
    delete_btn.add_css_class("destructive-action");
    let bar_box = gtk::Box::new(gtk::Orientation::Horizontal, 24);
    bar_box.set_halign(gtk::Align::Center);
    bar_box.append(&delete_btn);
    action_bar.set_center_widget(Some(&bar_box));

    let sel = multi_selection.clone();
    let st = store.clone();
    let exit = exit_selection.clone();
    delete_btn.connect_clicked(move |btn| {
        let n = sel.selection().size() as u32;
        if n == 0 {
            return;
        }

        // Collect selected album IDs.
        let mut ids = Vec::new();
        for i in 0..st.n_items() {
            if sel.is_selected(i) {
                if let Some(obj) = st
                    .item(i)
                    .and_then(|o| o.downcast::<AlbumItemObject>().ok())
                {
                    ids.push(AlbumId::from_raw(obj.id()));
                }
            }
        }

        let exit = exit.clone();
        let msg = if n == 1 {
            gettext("Delete 1 album?")
        } else {
            gettext("Delete {} albums?").replace("{}", &n.to_string())
        };

        let dialog = adw::AlertDialog::new(
            Some(&msg),
            Some(&gettext(
                "This cannot be undone. Photos in these albums will not be deleted.",
            )),
        );
        dialog.add_response("cancel", &gettext("Cancel"));
        dialog.add_response("delete", &gettext("Delete"));
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        dialog.connect_response(None, move |_, response| {
            if response != "delete" {
                return;
            }
            let album_client = MomentsApplication::default()
                .album_client_v2()
                .expect("album client v2 available");
            album_client.delete_album(ids.clone());
            exit.activate(None);
        });

        if let Some(win) = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
            dialog.present(Some(&win));
        }
    });
}
