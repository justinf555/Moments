use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use tracing::debug;

use crate::library::album::{Album, AlbumId};
use crate::library::media::MediaFilter;
use crate::library::Library;
use crate::ui::album_dialogs;
use crate::ui::photo_grid::model::PhotoGridModel;
use crate::ui::photo_grid::texture_cache::TextureCache;
use crate::ui::photo_grid::PhotoGridView;
use crate::ui::ContentView;

pub mod card;
pub mod factory;
pub mod item;

use item::AlbumItemObject;

/// Sort order for the album grid.
/// Values match the GSettings `album-sort-order` key.
const SORT_RECENT: u32 = 0;
const SORT_NAME: u32 = 1;
const SORT_CREATED: u32 = 2;

/// Grid view displaying all user albums as cards.
pub struct AlbumGridView {
    widget: gtk::Widget,
    store: gio::ListStore,
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
    sort_order: Rc<Cell<u32>>,
}

impl std::fmt::Debug for AlbumGridView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlbumGridView").finish_non_exhaustive()
    }
}

impl AlbumGridView {
    pub fn new(
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        settings: gio::Settings,
        texture_cache: Rc<TextureCache>,
        bus_sender: crate::event_bus::EventSender,
    ) -> Self {
        // ── Sort state ──────────────────────────────────────────────────
        let sort_order = Rc::new(Cell::new(settings.uint("album-sort-order")));

        // ── Headerbar ───────────────────────────────────────────────────
        let header = adw::HeaderBar::new();

        let new_album_btn = gtk::Button::with_label(&gettext("New Album"));
        new_album_btn.add_css_class("outlined");
        header.pack_start(&new_album_btn);

        // Overflow menu (⋮) with sort options.
        let sort_menu = build_sort_menu();
        let menu_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text(&gettext("Menu"))
            .menu_model(&sort_menu)
            .build();
        menu_btn.add_css_class("flat");
        header.pack_end(&menu_btn);

        // Sort action group — radio action with u32 state.
        let sort_action = gio::SimpleAction::new_stateful(
            "sort",
            Some(&u32::static_variant_type()),
            &sort_order.get().to_variant(),
        );

        let action_group = gio::SimpleActionGroup::new();
        action_group.add_action(&sort_action);

        // ── Selection mode state ────────────────────────────────────────
        let selection_mode = Rc::new(Cell::new(false));

        // ── Selection mode header widgets (hidden by default) ───────────
        let cancel_btn = gtk::Button::with_label(&gettext("Cancel"));
        cancel_btn.add_css_class("outlined");
        cancel_btn.set_visible(false);
        header.pack_start(&cancel_btn);

        let selection_title = gtk::Label::new(Some("0 selected"));
        selection_title.add_css_class("heading");
        selection_title.set_visible(false);

        // ── Grid ────────────────────────────────────────────────────────
        let store = gio::ListStore::new::<AlbumItemObject>();
        let multi_selection = gtk::MultiSelection::new(Some(store.clone()));

        let grid_view = gtk::GridView::new(
            Some(multi_selection.clone()),
            Some(factory::build_factory(
                Arc::clone(&library),
                tokio.clone(),
                Rc::clone(&selection_mode),
                multi_selection.clone(),
            )),
        );
        grid_view.set_min_columns(2);
        grid_view.set_max_columns(8);

        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
        scrolled.set_vexpand(true);
        scrolled.set_child(Some(&grid_view));

        // ── Action bar (bottom, selection mode only) ────────────────────
        let action_bar = gtk::ActionBar::new();
        action_bar.set_revealed(false);
        let delete_selected_btn = gtk::Button::with_label(&gettext("Delete Albums"));
        delete_selected_btn.add_css_class("destructive-action");
        let bar_box = gtk::Box::new(gtk::Orientation::Horizontal, 24);
        bar_box.set_halign(gtk::Align::Center);
        bar_box.append(&delete_selected_btn);
        action_bar.set_center_widget(Some(&bar_box));

        // ── Empty state ─────────────────────────────────────────────────
        let empty_page = adw::StatusPage::builder()
            .icon_name("folder-symbolic")
            .title(&gettext("No Albums Yet"))
            .description(&gettext(
                "Create an album to start organising your photos into collections.",
            ))
            .vexpand(true)
            .build();

        let empty_new_btn = gtk::Button::builder()
            .label(&gettext("New Album"))
            .halign(gtk::Align::Center)
            .build();
        empty_new_btn.add_css_class("pill");
        empty_new_btn.add_css_class("suggested-action");
        empty_page.set_child(Some(&empty_new_btn));

        // Stack to switch between grid and empty state.
        let content_stack = gtk::Stack::new();
        content_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        content_stack.add_named(&scrolled, Some("grid"));
        content_stack.add_named(&empty_page, Some("empty"));
        content_stack.set_visible_child_name("empty");

        // Wrap grid + action bar in a vertical box.
        let grid_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        grid_box.append(&content_stack);
        grid_box.append(&action_bar);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&grid_box));
        toolbar_view.insert_action_group("album", Some(&action_group));

        let grid_page = adw::NavigationPage::builder()
            .tag("albums")
            .title(&gettext("Albums"))
            .child(&toolbar_view)
            .build();

        let nav_view = adw::NavigationView::new();
        nav_view.push(&grid_page);

        let widget = nav_view.clone().upcast::<gtk::Widget>();

        // Remove drill-down view actions when popping back to the album grid.
        nav_view.connect_popped(|nav, _page| {
            let is_albums = nav
                .visible_page()
                .and_then(|p| p.tag())
                .map(|t| t == "albums")
                .unwrap_or(false);
            if is_albums {
                if let Some(win) = nav.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    win.insert_action_group("view", None::<&gtk::gio::SimpleActionGroup>);
                }
            }
        });

        // ── Toggle empty ↔ grid based on store count ────────────────────
        {
            let stack = content_stack.clone();
            store.connect_items_changed(move |store, _, _, _| {
                let target = if store.n_items() > 0 { "grid" } else { "empty" };
                stack.set_visible_child_name(target);
            });
        }

        // ── Wire sort action ────────────────────────────────────────────
        {
            let so = Rc::clone(&sort_order);
            let s = settings.clone();
            let st = store.clone();
            sort_action.connect_activate(move |action, param| {
                let Some(value) = param.and_then(|v| v.get::<u32>()) else { return };
                action.set_state(&value.to_variant());
                so.set(value);
                s.set_uint("album-sort-order", value).ok();
                sort_store(&st, value);
                debug!(sort_order = value, "album sort changed");
            });
        }

        // ── Enter/exit selection mode ────────────────────────────────────
        let enter_selection = gio::SimpleAction::new("select", None);
        {
            let sm = Rc::clone(&selection_mode);
            let new_btn = new_album_btn.clone();
            let menu = menu_btn.clone();
            let cancel = cancel_btn.clone();
            let title = selection_title.clone();
            let bar = action_bar.clone();
            let gv = grid_view.clone();
            enter_selection.connect_activate(move |_, _| {
                sm.set(true);
                new_btn.set_visible(false);
                menu.set_visible(false);
                cancel.set_visible(true);
                title.set_visible(true);
                title.set_text("0 selected");
                bar.set_revealed(true);
                // Show checkboxes on all visible cards.
                let mut child = gv.first_child();
                while let Some(c) = child {
                    if let Some(card) = c.first_child()
                        .and_then(|w| w.downcast::<card::AlbumCard>().ok())
                    {
                        card.set_selection_mode(true);
                    }
                    child = c.next_sibling();
                }
            });
        }
        action_group.add_action(&enter_selection);

        let exit_selection = {
            let sm = Rc::clone(&selection_mode);
            let new_btn = new_album_btn.clone();
            let menu = menu_btn.clone();
            let cancel = cancel_btn.clone();
            let title = selection_title.clone();
            let bar = action_bar.clone();
            let gv = grid_view.clone();
            let sel = multi_selection.clone();
            let action = gio::SimpleAction::new("exit-selection", None);
            action.connect_activate(move |_, _| {
                sm.set(false);
                new_btn.set_visible(true);
                menu.set_visible(true);
                cancel.set_visible(false);
                title.set_visible(false);
                bar.set_revealed(false);
                sel.unselect_all();
                // Hide checkboxes on all visible cards.
                let mut child = gv.first_child();
                while let Some(c) = child {
                    if let Some(card) = c.first_child()
                        .and_then(|w| w.downcast::<card::AlbumCard>().ok())
                    {
                        card.set_selection_mode(false);
                    }
                    child = c.next_sibling();
                }
            });
            action
        };

        // Cancel button → exit selection.
        {
            let exit = exit_selection.clone();
            cancel_btn.connect_clicked(move |_| { exit.activate(None); });
        }

        // Update selection count label when selection changes.
        {
            let title = selection_title.clone();
            multi_selection.connect_selection_changed(move |sel, _, _| {
                let count = sel.selection().size() as u32;
                title.set_text(&format!("{count} selected"));
            });
        }

        // Wire "Delete Albums" action bar button.
        {
            let sel = multi_selection.clone();
            let st = store.clone();
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let bs = bus_sender.clone();
            let exit = exit_selection.clone();
            let gv = grid_view.clone();
            delete_selected_btn.connect_clicked(move |btn| {
                let n = sel.selection().size() as u32;
                if n == 0 { return; }

                // Collect selected album IDs.
                let mut ids = Vec::new();
                for i in 0..st.n_items() {
                    if sel.is_selected(i) {
                        if let Some(obj) = st.item(i).and_then(|o| o.downcast::<AlbumItemObject>().ok()) {
                            ids.push(obj.album().id.as_str().to_owned());
                        }
                    }
                }

                let lib = Arc::clone(&lib);
                let tk = tk.clone();
                let bs = bs.clone();
                let exit = exit.clone();
                let msg = if n == 1 {
                    gettext("Delete 1 album?")
                } else {
                    gettext("Delete {} albums?").replace("{}", &n.to_string())
                };

                let dialog = adw::AlertDialog::new(Some(&msg), Some(&gettext("This cannot be undone. Photos in these albums will not be deleted.")));
                dialog.add_response("cancel", &gettext("Cancel"));
                dialog.add_response("delete", &gettext("Delete"));
                dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");

                let ids_clone = ids.clone();
                dialog.connect_response(None, move |_, response| {
                    if response != "delete" { return; }
                    let lib = Arc::clone(&lib);
                    let tk = tk.clone();
                    let bs = bs.clone();
                    let exit = exit.clone();
                    let ids = ids_clone.clone();
                    glib::MainContext::default().spawn_local(async move {
                        for aid in &ids {
                            let lib = Arc::clone(&lib);
                            let id = AlbumId::from_raw(aid.clone());
                            match tk.spawn(async move { lib.delete_album(&id).await }).await {
                                Ok(Ok(())) => {
                                    debug!(album_id = %aid, "album deleted (batch)");
                                    bs.send(crate::app_event::AppEvent::AlbumDeleted {
                                        id: AlbumId::from_raw(aid.clone()),
                                    });
                                }
                                Ok(Err(e)) => tracing::error!("failed to delete album {aid}: {e}"),
                                Err(e) => tracing::error!("tokio join error: {e}"),
                            }
                        }
                        exit.activate(None);
                    });
                });

                if let Some(win) = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    dialog.present(Some(&win));
                }
            });
        }

        // ── Wire "New Album" buttons ────────────────────────────────────
        {
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let bs = bus_sender.clone();
            let connect_create = move |btn: &gtk::Button| {
                let lib = Arc::clone(&lib);
                let tk = tk.clone();
                let bs = bs.clone();
                album_dialogs::show_create_album_dialog(
                    btn,
                    move |name| {
                        let lib = Arc::clone(&lib);
                        let tk = tk.clone();
                        let bs = bs.clone();
                        glib::MainContext::default().spawn_local(async move {
                            let n = name.clone();
                            match tk.spawn(async move { lib.create_album(&n).await }).await {
                                Ok(Ok(id)) => {
                                    debug!(album_id = %id, name = %name, "album created from albums view");
                                    bs.send(crate::app_event::AppEvent::AlbumCreated {
                                        id,
                                        name,
                                    });
                                }
                                Ok(Err(e)) => {
                                    tracing::error!("failed to create album: {e}");
                                }
                                Err(e) => tracing::error!("tokio join error: {e}"),
                            }
                        });
                    },
                );
            };

            let cb = connect_create.clone();
            new_album_btn.connect_clicked(move |btn| cb(btn));
            empty_new_btn.connect_clicked(move |btn| connect_create(btn));
        }

        // ── Wire item activation (click → open album photo grid) ────────
        {
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let s = settings.clone();
            let tc = Rc::clone(&texture_cache);
            let bs = bus_sender.clone();
            let st = store.clone();
            let nav = nav_view.clone();

            grid_view.connect_activate(move |_, position| {
                let Some(obj) = st.item(position) else { return };
                let Some(item) = obj.downcast_ref::<AlbumItemObject>() else { return };
                let album = item.album();
                let album_id = AlbumId::from_raw(album.id.as_str().to_owned());
                let album_name = album.name.clone();

                debug!(album_id = %album.id, name = %album_name, "album activated");

                let model = Rc::new(PhotoGridModel::new(
                    Arc::clone(&lib),
                    tk.clone(),
                    MediaFilter::Album { album_id },
                    bs.clone(),
                ));
                let view = Rc::new(PhotoGridView::new(
                    Arc::clone(&lib),
                    tk.clone(),
                    s.clone(),
                    Rc::clone(&tc),
                    bs.clone(),
                ));
                view.set_model(Rc::clone(&model));
                model.subscribe_to_bus();

                let page = adw::NavigationPage::builder()
                    .tag("album-detail")
                    .title(&album_name)
                    .child(view.widget())
                    .build();

                if let Some(actions) = view.view_actions() {
                    if let Some(win) = nav.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                        win.insert_action_group("view", Some(actions));
                    }
                }

                nav.push(&page);
            });
        }

        // ── Right-click context menu ────────────────────────────────────
        {
            let gesture = gtk::GestureClick::new();
            gesture.set_button(3);

            let gv = grid_view.clone();
            let store_ctx = store.clone();
            let lib_ctx = Arc::clone(&library);
            let tk_ctx = tokio.clone();
            let nav_ctx = nav_view.clone();
            let s_ctx = settings.clone();
            let tc_ctx = Rc::clone(&texture_cache);
            let bs_ctx = bus_sender.clone();

            gesture.connect_pressed(move |gesture, _, x, y| {
                // Find which grid item was clicked.
                let Some(picked) = gv.pick(x, y, gtk::PickFlags::DEFAULT) else {
                    return;
                };

                let grid_widget = gv.upcast_ref::<gtk::Widget>();
                let mut target = Some(picked);
                while let Some(ref w) = target {
                    if w.parent().as_ref() == Some(grid_widget) {
                        break;
                    }
                    target = w.parent();
                }
                let Some(target) = target else { return };

                let mut pos = 0u32;
                let mut child = gv.first_child();
                loop {
                    let Some(c) = child else { return };
                    if c == target {
                        break;
                    }
                    pos += 1;
                    child = c.next_sibling();
                }

                let Some(obj) = store_ctx
                    .item(pos)
                    .and_then(|o| o.downcast::<AlbumItemObject>().ok())
                else {
                    return;
                };

                let album = obj.album();
                let album_id_str = album.id.as_str().to_owned();
                let album_name = album.name.clone();

                // Build popover menu.
                let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
                vbox.set_margin_top(6);
                vbox.set_margin_bottom(6);
                vbox.set_margin_start(6);
                vbox.set_margin_end(6);

                let popover = gtk::Popover::new();

                // Open
                let open_btn = gtk::Button::with_label(&gettext("Open"));
                open_btn.add_css_class("flat");
                vbox.append(&open_btn);

                // Rename
                let rename_btn = gtk::Button::with_label(&gettext("Rename…"));
                rename_btn.add_css_class("flat");
                vbox.append(&rename_btn);

                // Separator
                vbox.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

                // Pin to sidebar (stub — disabled)
                let pin_btn = gtk::Button::with_label(&gettext("Pin to Sidebar"));
                pin_btn.add_css_class("flat");
                pin_btn.set_sensitive(false);
                pin_btn.set_tooltip_text(Some(&gettext("Coming soon")));
                vbox.append(&pin_btn);

                // Share (stub)
                let share_btn = gtk::Button::with_label(&gettext("Share…"));
                share_btn.add_css_class("flat");
                share_btn.set_sensitive(false);
                vbox.append(&share_btn);

                // Separator
                vbox.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

                // Delete (destructive)
                let delete_btn = gtk::Button::with_label(&gettext("Delete Album…"));
                delete_btn.add_css_class("flat");
                delete_btn.add_css_class("error");
                vbox.append(&delete_btn);

                popover.set_child(Some(&vbox));
                popover.set_parent(&gv);
                popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                popover.set_has_arrow(true);

                // Wire Open — same as activation.
                {
                    let pop = popover.downgrade();
                    let lib = Arc::clone(&lib_ctx);
                    let tk = tk_ctx.clone();
                    let s = s_ctx.clone();
                    let tc = Rc::clone(&tc_ctx);
                    let bs = bs_ctx.clone();
                    let nav = nav_ctx.clone();
                    let aid = album_id_str.clone();
                    let aname = album_name.clone();
                    open_btn.connect_clicked(move |_| {
                        if let Some(p) = pop.upgrade() { p.popdown(); }
                        let album_id = AlbumId::from_raw(aid.clone());
                        let model = Rc::new(PhotoGridModel::new(
                            Arc::clone(&lib), tk.clone(),
                            MediaFilter::Album { album_id }, bs.clone(),
                        ));
                        let view = Rc::new(PhotoGridView::new(
                            Arc::clone(&lib), tk.clone(), s.clone(),
                            Rc::clone(&tc), bs.clone(),
                        ));
                        view.set_model(Rc::clone(&model));
                        model.subscribe_to_bus();
                        let page = adw::NavigationPage::builder()
                            .tag("album-detail").title(&aname)
                            .child(view.widget()).build();
                        if let Some(actions) = view.view_actions() {
                            if let Some(win) = nav.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                                win.insert_action_group("view", Some(actions));
                            }
                        }
                        nav.push(&page);
                    });
                }

                // Wire Rename.
                {
                    let pop = popover.downgrade();
                    let lib = Arc::clone(&lib_ctx);
                    let tk = tk_ctx.clone();
                    let bs = bs_ctx.clone();
                    let aid = album_id_str.clone();
                    let aname = album_name.clone();
                    let gv_ref = gv.clone();
                    rename_btn.connect_clicked(move |_| {
                        if let Some(p) = pop.upgrade() { p.popdown(); }
                        let lib = Arc::clone(&lib);
                        let tk = tk.clone();
                        let bs = bs.clone();
                        let aid = aid.clone();
                        if let Some(win) = gv_ref.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                            album_dialogs::show_rename_album_dialog(&win, &aname, move |new_name| {
                                let lib = Arc::clone(&lib);
                                let tk = tk.clone();
                                let bs = bs.clone();
                                let aid = aid.clone();
                                glib::MainContext::default().spawn_local(async move {
                                    let n = new_name.clone();
                                    let id = AlbumId::from_raw(aid.clone());
                                    match tk.spawn(async move { lib.rename_album(&id, &n).await }).await {
                                        Ok(Ok(())) => {
                                            debug!(album_id = %aid, name = %new_name, "album renamed");
                                            bs.send(crate::app_event::AppEvent::AlbumRenamed {
                                                id: AlbumId::from_raw(aid),
                                                name: new_name,
                                            });
                                        }
                                        Ok(Err(e)) => tracing::error!("failed to rename album: {e}"),
                                        Err(e) => tracing::error!("tokio join error: {e}"),
                                    }
                                });
                            });
                        }
                    });
                }

                // Wire Delete.
                {
                    let pop = popover.downgrade();
                    let lib = Arc::clone(&lib_ctx);
                    let tk = tk_ctx.clone();
                    let bs = bs_ctx.clone();
                    let aid = album_id_str.clone();
                    let aname = album_name.clone();
                    let gv_ref = gv.clone();
                    delete_btn.connect_clicked(move |_| {
                        if let Some(p) = pop.upgrade() { p.popdown(); }
                        let lib = Arc::clone(&lib);
                        let tk = tk.clone();
                        let bs = bs.clone();
                        let aid = aid.clone();
                        if let Some(win) = gv_ref.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                            album_dialogs::show_delete_album_dialog(&win, &aname, move || {
                                let lib = Arc::clone(&lib);
                                let tk = tk.clone();
                                let bs = bs.clone();
                                let aid = aid.clone();
                                glib::MainContext::default().spawn_local(async move {
                                    let id = AlbumId::from_raw(aid.clone());
                                    match tk.spawn(async move { lib.delete_album(&id).await }).await {
                                        Ok(Ok(())) => {
                                            debug!(album_id = %aid, "album deleted");
                                            bs.send(crate::app_event::AppEvent::AlbumDeleted {
                                                id: AlbumId::from_raw(aid),
                                            });
                                        }
                                        Ok(Err(e)) => tracing::error!("failed to delete album: {e}"),
                                        Err(e) => tracing::error!("tokio join error: {e}"),
                                    }
                                });
                            });
                        }
                    });
                }

                popover.connect_closed(|p| { p.unparent(); });
                popover.popup();
                gesture.set_state(gtk::EventSequenceState::Claimed);
            });

            grid_view.add_controller(gesture);
        }

        // ── Load albums asynchronously ──────────────────────────────────
        let view = Self {
            widget,
            store: store.clone(),
            library: Arc::clone(&library),
            tokio: tokio.clone(),
            sort_order: Rc::clone(&sort_order),
        };

        {
            let st = store.clone();
            let so = Rc::clone(&sort_order);
            reload_albums(&st, &library, &tokio, so);
        }

        // ── Subscribe to bus for album changes ──────────────────────────
        {
            let st = store.clone();
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let so = Rc::clone(&sort_order);
            crate::event_bus::subscribe(move |event| {
                match event {
                    crate::app_event::AppEvent::AlbumCreated { .. }
                    | crate::app_event::AppEvent::AlbumRenamed { .. }
                    | crate::app_event::AppEvent::AlbumDeleted { .. } => {
                        reload_albums(&st, &lib, &tk, Rc::clone(&so));
                    }
                    _ => {}
                }
            });
        }

        view
    }

    pub fn reload(&self) {
        reload_albums(&self.store, &self.library, &self.tokio, Rc::clone(&self.sort_order));
    }
}

impl ContentView for AlbumGridView {
    fn widget(&self) -> &gtk::Widget {
        &self.widget
    }
}

/// Build the overflow menu model with sort radio actions.
fn build_sort_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let sort_section = gio::Menu::new();
    sort_section.append(Some(&gettext("Most Recent Photo")), Some("album.sort(uint32 0)"));
    sort_section.append(Some(&gettext("Name (A–Z)")), Some("album.sort(uint32 1)"));
    sort_section.append(Some(&gettext("Date Created")), Some("album.sort(uint32 2)"));
    menu.append_section(Some(&gettext("Sort by")), &sort_section);

    let select_section = gio::Menu::new();
    select_section.append(Some(&gettext("Select Albums")), Some("album.select"));
    menu.append_section(None, &select_section);

    menu
}

/// Sort the store in-place by the given sort order.
fn sort_store(store: &gio::ListStore, order: u32) {
    store.sort(|a, b| {
        let a = a.downcast_ref::<AlbumItemObject>().expect("store holds AlbumItemObject").album();
        let b = b.downcast_ref::<AlbumItemObject>().expect("store holds AlbumItemObject").album();
        sort_albums(a, b, order)
    });
}

/// Async-load all albums into the store, then sort.
fn reload_albums(
    store: &gio::ListStore,
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    sort_order: Rc<Cell<u32>>,
) {
    let lib = Arc::clone(library);
    let tk = tokio.clone();
    let store = store.clone();

    glib::MainContext::default().spawn_local(async move {
        let result = tk.spawn(async move { lib.list_albums().await }).await;
        match result {
            Ok(Ok(mut albums)) => {
                // Sort before building objects.
                let order = sort_order.get();
                albums.sort_by(|a, b| sort_albums(a, b, order));

                let objects: Vec<glib::Object> = albums
                    .into_iter()
                    .map(|a| AlbumItemObject::new(a).upcast())
                    .collect();
                store.splice(0, store.n_items(), &objects);
                debug!(count = store.n_items(), sort_order = order, "albums loaded");
            }
            Ok(Err(e)) => tracing::error!("failed to load albums: {e}"),
            Err(e) => tracing::error!("tokio join error loading albums: {e}"),
        }
    });
}

/// Compare two albums for sorting.
fn sort_albums(a: &Album, b: &Album, order: u32) -> std::cmp::Ordering {
    match order {
        SORT_NAME => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        SORT_CREATED => b.created_at.cmp(&a.created_at),
        _ => b.updated_at.cmp(&a.updated_at),
    }
}
