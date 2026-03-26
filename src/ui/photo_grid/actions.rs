use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use tracing::debug;

use crate::library::album::AlbumId;
use crate::library::media::{MediaFilter, MediaId};
use crate::library::Library;
use crate::ui::model_registry::ModelRegistry;

use super::item::MediaItemObject;

/// Shared context passed to all `wire_*` functions.
pub(super) struct ActionContext {
    pub selection: gtk::MultiSelection,
    pub library: Arc<dyn Library>,
    pub tokio: tokio::runtime::Handle,
    pub registry: Rc<ModelRegistry>,
    pub filter: MediaFilter,
    pub nav_view: adw::NavigationView,
    pub grid_view: gtk::GridView,
}

/// Run an async library action on the selected items.
///
/// 1. Collects selected `MediaId`s from the `MultiSelection`
/// 2. Clears the selection
/// 3. Optionally disables a button
/// 4. Spawns the async `action` on the Tokio executor
/// 5. Calls `on_success` on the GTK thread with the collected IDs
///
/// Both header bar buttons and context menu items use this instead of
/// duplicating ~30 lines each.
pub(super) fn run_action<F, Fut>(
    ctx: &ActionContext,
    disable_btn: Option<&gtk::Button>,
    action: F,
    on_success: impl Fn(&[MediaId], &Rc<ModelRegistry>) + 'static,
) where
    F: FnOnce(Arc<dyn Library>, Vec<MediaId>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), crate::library::error::LibraryError>> + Send + 'static,
{
    let ids = super::collect_selected_ids(&ctx.selection);
    if ids.is_empty() {
        return;
    }
    ctx.selection.unselect_all();
    if let Some(btn) = disable_btn {
        btn.set_sensitive(false);
    }

    let lib = Arc::clone(&ctx.library);
    let tk = ctx.tokio.clone();
    let reg = Rc::clone(&ctx.registry);
    let grid_weak = ctx.grid_view.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let ids_bc = ids.clone();
        let result = tk.spawn(async move { action(lib, ids).await }).await;
        match result {
            Ok(Ok(())) => on_success(&ids_bc, &reg),
            Ok(Err(e)) => {
                tracing::error!("action failed: {e}");
                if let Some(grid) = grid_weak.upgrade() {
                    let _ = grid.activate_action("win.show-toast", Some(&"Operation failed".to_variant()));
                }
            }
            Err(e) => tracing::error!("action join failed: {e}"),
        }
    });
}

/// Wire the selection-changed signal to enable/disable header bar buttons.
pub(super) fn wire_selection_buttons(
    ctx: &ActionContext,
    trash_btn: &gtk::Button,
    restore_btn: &gtk::Button,
    delete_btn: &gtk::Button,
    album_btn: &gtk::Button,
    remove_from_album_btn: &gtk::Button,
) {
    let is_trash_view = ctx.filter == MediaFilter::Trashed;
    let is_album_view = matches!(ctx.filter, MediaFilter::Album { .. });

    trash_btn.set_visible(!is_trash_view);
    restore_btn.set_visible(is_trash_view);
    delete_btn.set_visible(is_trash_view);
    album_btn.set_visible(!is_trash_view && !is_album_view);
    remove_from_album_btn.set_visible(is_album_view);

    // Clone for the selection_changed closure.
    {
        let trash_btn = trash_btn.clone();
        let restore_btn = restore_btn.clone();
        let delete_btn = delete_btn.clone();
        let album_btn = album_btn.clone();
        let remove_btn = remove_from_album_btn.clone();
        ctx.selection.connect_selection_changed(move |sel, _, _| {
            let has_selection = sel.selection().size() > 0;
            trash_btn.set_sensitive(has_selection);
            restore_btn.set_sensitive(has_selection);
            delete_btn.set_sensitive(has_selection);
            album_btn.set_sensitive(has_selection);
            remove_btn.set_sensitive(has_selection);
        });
    }

    if is_trash_view {
        wire_restore_button(ctx, restore_btn.clone());
        wire_delete_button(ctx, delete_btn.clone());
    } else {
        wire_trash_button(ctx, trash_btn.clone());
    }

    if is_album_view {
        if let MediaFilter::Album { album_id } = &ctx.filter {
            wire_remove_from_album_button(ctx, remove_from_album_btn.clone(), album_id.clone());
        }
    }
}

fn wire_restore_button(ctx: &ActionContext, btn: gtk::Button) {
    let ctx_sel = ctx.selection.clone();
    let ctx_lib = Arc::clone(&ctx.library);
    let ctx_tk = ctx.tokio.clone();
    let ctx_reg = Rc::clone(&ctx.registry);
    btn.connect_clicked(move |btn| {
        let ctx = ActionContext {
            selection: ctx_sel.clone(),
            library: Arc::clone(&ctx_lib),
            tokio: ctx_tk.clone(),
            registry: Rc::clone(&ctx_reg),
            filter: MediaFilter::Trashed,
            nav_view: adw::NavigationView::new(),  // unused by run_action
            grid_view: gtk::GridView::new(None::<gtk::NoSelection>, None::<gtk::SignalListItemFactory>),
        };
        run_action(&ctx, Some(btn), |lib, ids| async move { lib.restore(&ids).await }, |ids, reg| {
            for id in ids {
                reg.on_trashed(id, false);
            }
        });
    });
}

fn wire_delete_button(ctx: &ActionContext, btn: gtk::Button) {
    let ctx_sel = ctx.selection.clone();
    let ctx_lib = Arc::clone(&ctx.library);
    let ctx_tk = ctx.tokio.clone();
    let ctx_reg = Rc::clone(&ctx.registry);
    let nav_view = ctx.nav_view.clone();
    btn.connect_clicked(move |btn| {
        let ids = super::collect_selected_ids(&ctx_sel);
        if ids.is_empty() {
            return;
        }

        let count = ids.len();
        let dialog = adw::AlertDialog::builder()
            .heading("Delete Permanently?")
            .body(format!(
                "This will permanently delete {count} {} and cannot be undone.",
                if count == 1 { "photo" } else { "photos" }
            ))
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("delete", "Delete");
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let sel = ctx_sel.clone();
        let lib = Arc::clone(&ctx_lib);
        let tk = ctx_tk.clone();
        let reg = Rc::clone(&ctx_reg);
        let btn = btn.clone();
        dialog.connect_response(None, move |_, response| {
            if response != "delete" {
                return;
            }
            let ids = ids.clone();
            sel.unselect_all();
            btn.set_sensitive(false);

            let lib = Arc::clone(&lib);
            let tk = tk.clone();
            let reg = Rc::clone(&reg);
            let btn_toast = btn.clone();
            glib::MainContext::default().spawn_local(async move {
                let ids_bc = ids.clone();
                let result = tk
                    .spawn(async move { lib.delete_permanently(&ids).await })
                    .await;
                match result {
                    Ok(Ok(())) => {
                        for id in &ids_bc {
                            reg.on_deleted(id);
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::error!("delete_permanently failed: {e}");
                        let _ = btn_toast.activate_action("win.show-toast", Some(&"Failed to delete permanently".to_variant()));
                    }
                    Err(e) => tracing::error!("delete_permanently join failed: {e}"),
                }
            });
        });
        dialog.present(
            nav_view
                .root()
                .as_ref()
                .and_then(|r| r.downcast_ref::<gtk::Window>()),
        );
    });
}

fn wire_trash_button(ctx: &ActionContext, btn: gtk::Button) {
    let ctx_sel = ctx.selection.clone();
    let ctx_lib = Arc::clone(&ctx.library);
    let ctx_tk = ctx.tokio.clone();
    let ctx_reg = Rc::clone(&ctx.registry);
    btn.connect_clicked(move |btn| {
        let ctx = ActionContext {
            selection: ctx_sel.clone(),
            library: Arc::clone(&ctx_lib),
            tokio: ctx_tk.clone(),
            registry: Rc::clone(&ctx_reg),
            filter: MediaFilter::All,
            nav_view: adw::NavigationView::new(),
            grid_view: gtk::GridView::new(None::<gtk::NoSelection>, None::<gtk::SignalListItemFactory>),
        };
        run_action(&ctx, Some(btn), |lib, ids| async move { lib.trash(&ids).await }, |ids, reg| {
            for id in ids {
                reg.on_trashed(id, true);
            }
        });
    });
}

fn wire_remove_from_album_button(ctx: &ActionContext, btn: gtk::Button, album_id: AlbumId) {
    let ctx_sel = ctx.selection.clone();
    let ctx_lib = Arc::clone(&ctx.library);
    let ctx_tk = ctx.tokio.clone();
    let ctx_reg = Rc::clone(&ctx.registry);
    btn.connect_clicked(move |btn| {
        let ctx = ActionContext {
            selection: ctx_sel.clone(),
            library: Arc::clone(&ctx_lib),
            tokio: ctx_tk.clone(),
            registry: Rc::clone(&ctx_reg),
            filter: MediaFilter::All,
            nav_view: adw::NavigationView::new(),
            grid_view: gtk::GridView::new(None::<gtk::NoSelection>, None::<gtk::SignalListItemFactory>),
        };
        let aid = album_id.clone();
        run_action(
            &ctx,
            Some(btn),
            move |lib, ids| async move { lib.remove_from_album(&aid, &ids).await },
            {
                let aid = album_id.clone();
                move |_, reg| {
                    tracing::debug!(album_id = %aid, "photos removed from album");
                    reg.on_album_media_changed(&aid);
                }
            },
        );
    });
}

/// Wire the "Add to Album" button popover.
pub(super) fn wire_album_controls(ctx: &ActionContext, album_btn: &gtk::Button) {
    let lib = Arc::clone(&ctx.library);
    let tk = ctx.tokio.clone();
    let reg = Rc::clone(&ctx.registry);
    let selection = ctx.selection.clone();

    album_btn.connect_clicked(move |btn: &gtk::Button| {
        debug!("album button clicked, loading albums async");

        let lib = Arc::clone(&lib);
        let tk = tk.clone();
        let reg = Rc::clone(&reg);
        let sel = selection.clone();
        let btn_weak: glib::WeakRef<gtk::Button> = btn.downgrade();

        glib::MainContext::default().spawn_local(async move {
            let lib_q = Arc::clone(&lib);
            debug!("fetching album list from library");
            let albums = match tk.spawn(async move { lib_q.list_albums().await }).await {
                Ok(Ok(a)) => a,
                Ok(Err(e)) => {
                    tracing::error!("list_albums failed: {e}");
                    return;
                }
                Err(e) => {
                    tracing::error!("list_albums join failed: {e}");
                    return;
                }
            };

            let Some(btn) = btn_weak.upgrade() else {
                debug!("album button weak ref gone");
                return;
            };

            debug!(count = albums.len(), "albums loaded, building popover");

            let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
            vbox.set_margin_top(6);
            vbox.set_margin_bottom(6);
            vbox.set_margin_start(6);
            vbox.set_margin_end(6);

            let popover = gtk::Popover::new();
            popover.set_parent(btn.upcast_ref::<gtk::Widget>());

            if albums.is_empty() {
                let label = gtk::Label::new(Some("No albums"));
                label.add_css_class("dim-label");
                vbox.append(&label);
            } else {
                for album in &albums {
                    let ab = gtk::Button::with_label(&album.name);
                    ab.add_css_class("flat");
                    let aid = album.id.clone();
                    let lib_add = Arc::clone(&lib);
                    let tk_add = tk.clone();
                    let reg_add = Rc::clone(&reg);
                    let sel_add = sel.clone();
                    let pop_weak = popover.downgrade();
                    ab.connect_clicked(move |_| {
                        debug!(album_id = %aid, "album selected in popover");
                        let ids = super::collect_selected_ids(&sel_add);
                        if ids.is_empty() {
                            debug!("no photos selected, skipping");
                            return;
                        }
                        debug!(count = ids.len(), album_id = %aid, "adding photos to album");

                        if let Some(p) = pop_weak.upgrade() {
                            p.popdown();
                        }

                        let lib = Arc::clone(&lib_add);
                        let tk = tk_add.clone();
                        let reg = Rc::clone(&reg_add);
                        let aid = aid.clone();
                        let pop_toast = pop_weak.clone();
                        glib::MainContext::default().spawn_local(async move {
                            let aid_bc = aid.clone();
                            let result = tk
                                .spawn(async move { lib.add_to_album(&aid, &ids).await })
                                .await;
                            match result {
                                Ok(Ok(())) => {
                                    debug!(album_id = %aid_bc, "photos added to album");
                                    reg.on_album_media_changed(&aid_bc);
                                }
                                Ok(Err(e)) => {
                                    tracing::error!("add_to_album failed: {e}");
                                    if let Some(p) = pop_toast.upgrade() {
                                        let _ = p.activate_action("win.show-toast", Some(&"Failed to add to album".to_variant()));
                                    }
                                }
                                Err(e) => tracing::error!("add_to_album join failed: {e}"),
                            }
                        });
                    });
                    vbox.append(&ab);
                }
            }

            popover.set_child(Some(&vbox));

            popover.connect_closed(move |p| {
                debug!("album popover closed");
                p.unparent();
            });

            debug!("showing album popover");
            popover.popup();
        });
    });
}

/// Wire the right-click context menu on grid cells.
pub(super) fn wire_context_menu(ctx: &ActionContext) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);

    let grid_view = ctx.grid_view.clone();
    let selection = ctx.selection.clone();
    let lib = Arc::clone(&ctx.library);
    let tk = ctx.tokio.clone();
    let reg = Rc::clone(&ctx.registry);
    let filter = ctx.filter.clone();

    gesture.connect_pressed(move |gesture, _, x, y| {
        let Some(picked) = grid_view.pick(x, y, gtk::PickFlags::DEFAULT) else {
            return;
        };

        let grid_widget = grid_view.upcast_ref::<gtk::Widget>();
        let mut target = Some(picked);
        while let Some(ref w) = target {
            if w.parent().as_ref() == Some(grid_widget) {
                break;
            }
            target = w.parent();
        }
        let Some(target) = target else { return };

        let mut pos = 0u32;
        let mut child = grid_view.first_child();
        loop {
            let Some(c) = child else { return };
            if c == target {
                break;
            }
            pos += 1;
            child = c.next_sibling();
        }

        if !selection.is_selected(pos) {
            selection.unselect_all();
            selection.select_item(pos, true);
        }

        let Some(obj) = selection
            .item(pos)
            .and_then(|o| o.downcast::<MediaItemObject>().ok())
        else {
            return;
        };

        let is_favorite = obj.is_favorite();
        let is_trash = matches!(filter, MediaFilter::Trashed);
        let is_album = matches!(filter, MediaFilter::Album { .. });

        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        vbox.set_margin_top(6);
        vbox.set_margin_bottom(6);
        vbox.set_margin_start(6);
        vbox.set_margin_end(6);

        let popover = gtk::Popover::new();
        let pop_ref: glib::WeakRef<gtk::Popover> = popover.downgrade();

        let ctx = ActionContext {
            selection: selection.clone(),
            library: Arc::clone(&lib),
            tokio: tk.clone(),
            registry: Rc::clone(&reg),
            filter: filter.clone(),
            nav_view: adw::NavigationView::new(), // unused by run_action_popover
            grid_view: gtk::GridView::new(None::<gtk::NoSelection>, None::<gtk::SignalListItemFactory>),
        };

        if is_trash {
            let restore_btn = gtk::Button::with_label("Restore");
            restore_btn.add_css_class("flat");
            vbox.append(&restore_btn);

            let delete_btn = gtk::Button::with_label("Delete Permanently");
            delete_btn.add_css_class("flat");
            delete_btn.add_css_class("error");
            vbox.append(&delete_btn);

            {
                let pw = pop_ref.clone();
                let sel = ctx.selection.clone();
                let lib = Arc::clone(&ctx.library);
                let tk = ctx.tokio.clone();
                let reg = Rc::clone(&ctx.registry);
                restore_btn.connect_clicked(move |_| {
                    if let Some(p) = pw.upgrade() {
                        p.popdown();
                    }
                    let ids = super::collect_selected_ids(&sel);
                    if ids.is_empty() {
                        return;
                    }
                    sel.unselect_all();
                    let lib = Arc::clone(&lib);
                    let tk = tk.clone();
                    let reg = Rc::clone(&reg);
                    glib::MainContext::default().spawn_local(async move {
                        let ids_bc = ids.clone();
                        if let Ok(Ok(())) =
                            tk.spawn(async move { lib.restore(&ids).await }).await
                        {
                            for id in &ids_bc {
                                reg.on_trashed(id, false);
                            }
                        }
                    });
                });
            }

            {
                let pw = pop_ref.clone();
                let sel = ctx.selection.clone();
                let lib = Arc::clone(&ctx.library);
                let tk = ctx.tokio.clone();
                let reg = Rc::clone(&ctx.registry);
                delete_btn.connect_clicked(move |_| {
                    if let Some(p) = pw.upgrade() {
                        p.popdown();
                    }
                    let ids = super::collect_selected_ids(&sel);
                    if ids.is_empty() {
                        return;
                    }
                    sel.unselect_all();
                    let lib = Arc::clone(&lib);
                    let tk = tk.clone();
                    let reg = Rc::clone(&reg);
                    glib::MainContext::default().spawn_local(async move {
                        let ids_bc = ids.clone();
                        if let Ok(Ok(())) = tk
                            .spawn(async move { lib.delete_permanently(&ids).await })
                            .await
                        {
                            for id in &ids_bc {
                                reg.on_deleted(id);
                            }
                        }
                    });
                });
            }
        } else {
            let fav_label = if is_favorite {
                "Unfavourite"
            } else {
                "Favourite"
            };
            let fav_btn = gtk::Button::with_label(fav_label);
            fav_btn.add_css_class("flat");
            vbox.append(&fav_btn);

            let trash_ctx_btn = gtk::Button::with_label("Move to Trash");
            trash_ctx_btn.add_css_class("flat");
            trash_ctx_btn.add_css_class("error");
            vbox.append(&trash_ctx_btn);

            if is_album {
                let remove_btn = gtk::Button::with_label("Remove from Album");
                remove_btn.add_css_class("flat");
                vbox.append(&remove_btn);

                if let MediaFilter::Album { ref album_id } = filter {
                    let pw = pop_ref.clone();
                    let sel = ctx.selection.clone();
                    let lib = Arc::clone(&ctx.library);
                    let tk = ctx.tokio.clone();
                    let reg = Rc::clone(&ctx.registry);
                    let aid = album_id.clone();
                    remove_btn.connect_clicked(move |_| {
                        if let Some(p) = pw.upgrade() {
                            p.popdown();
                        }
                        let ids = super::collect_selected_ids(&sel);
                        if ids.is_empty() {
                            return;
                        }
                        sel.unselect_all();
                        let lib = Arc::clone(&lib);
                        let tk = tk.clone();
                        let reg = Rc::clone(&reg);
                        let aid = aid.clone();
                        glib::MainContext::default().spawn_local(async move {
                            let aid_bc = aid.clone();
                            if let Ok(Ok(())) = tk
                                .spawn(async move { lib.remove_from_album(&aid, &ids).await })
                                .await
                            {
                                reg.on_album_media_changed(&aid_bc);
                            }
                        });
                    });
                }
            }

            let new_fav = !is_favorite;
            {
                let pw = pop_ref.clone();
                let sel = ctx.selection.clone();
                let lib = Arc::clone(&ctx.library);
                let tk = ctx.tokio.clone();
                let reg = Rc::clone(&ctx.registry);
                fav_btn.connect_clicked(move |_| {
                    if let Some(p) = pw.upgrade() {
                        p.popdown();
                    }
                    let ids = super::collect_selected_ids(&sel);
                    if ids.is_empty() {
                        return;
                    }
                    let lib = Arc::clone(&lib);
                    let tk = tk.clone();
                    let reg = Rc::clone(&reg);
                    glib::MainContext::default().spawn_local(async move {
                        let ids_bc = ids.clone();
                        if let Ok(Ok(())) = tk
                            .spawn(async move { lib.set_favorite(&ids, new_fav).await })
                            .await
                        {
                            for id in &ids_bc {
                                reg.on_favorite_changed(id, new_fav);
                            }
                        }
                    });
                });
            }

            {
                let pw = pop_ref.clone();
                let sel = ctx.selection.clone();
                let lib = Arc::clone(&ctx.library);
                let tk = ctx.tokio.clone();
                let reg = Rc::clone(&ctx.registry);
                trash_ctx_btn.connect_clicked(move |_| {
                    if let Some(p) = pw.upgrade() {
                        p.popdown();
                    }
                    let ids = super::collect_selected_ids(&sel);
                    if ids.is_empty() {
                        return;
                    }
                    sel.unselect_all();
                    let lib = Arc::clone(&lib);
                    let tk = tk.clone();
                    let reg = Rc::clone(&reg);
                    glib::MainContext::default().spawn_local(async move {
                        let ids_bc = ids.clone();
                        if let Ok(Ok(())) =
                            tk.spawn(async move { lib.trash(&ids).await }).await
                        {
                            for id in &ids_bc {
                                reg.on_trashed(id, true);
                            }
                        }
                    });
                });
            }
        }

        popover.set_child(Some(&vbox));
        popover.set_parent(&grid_view);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            x as i32, y as i32, 1, 1,
        )));
        popover.set_has_arrow(true);

        popover.connect_closed(move |p| {
            p.unparent();
        });

        popover.popup();
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });

    ctx.grid_view.add_controller(gesture);
}
