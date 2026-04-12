//! Proof-of-concept: headless integration tests for Moments.
//!
//! Validates that GTK4/libadwaita widgets can be instantiated and tested
//! without a physical display, using either `mutter --headless` (Wayland)
//! or `xvfb-run` (X11 fallback).
//!
//! Run with:
//!   cargo test --features integration-tests --test headless_poc -- --test-threads=1
//!
//! Headless (no desktop):
//!   dbus-run-session mutter --headless --wayland --no-x11 --virtual-monitor 1024x768 -- \
//!     cargo test --features integration-tests --test headless_poc -- --test-threads=1

#![cfg(feature = "integration-tests")]

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::prelude::*;

/// Process all pending GLib main loop events.
fn flush_events() {
    while glib::MainContext::default().iteration(false) {}
}

// ── Displayless: event fan-out pattern (no compositor needed) ────────────────

#[cfg(test)]
mod event_fan_out {
    use super::*;

    /// Simulates the EventBus fan-out pattern from the design doc.
    /// Uses glib::idle_add_local (the existing pattern) to deliver
    /// events from a background thread to the GTK main loop.
    #[gtk::test]
    fn fan_out_delivers_to_all_subscribers() {
        let subscribers: Rc<std::cell::RefCell<Vec<Box<dyn Fn(&str)>>>> =
            Rc::new(std::cell::RefCell::new(Vec::new()));

        // Register 3 subscribers
        let count = Rc::new(Cell::new(0u32));
        for _ in 0..3 {
            let c = Rc::clone(&count);
            subscribers.borrow_mut().push(Box::new(move |_msg| {
                c.set(c.get() + 1);
            }));
        }

        // Simulate event delivery via idle_add_local (like the current app does)
        let subs = Rc::clone(&subscribers);
        glib::idle_add_local_once(move || {
            let msg = "ThumbnailReady";
            for handler in subs.borrow().iter() {
                handler(msg);
            }
        });

        flush_events();

        assert_eq!(count.get(), 3, "all 3 subscribers should receive the event");
    }

    /// Verifies cross-thread event delivery via mpsc + idle_add_local
    /// (the pattern used by LibraryEvent today).
    #[gtk::test]
    fn cross_thread_event_delivery() {
        let (tx, rx) = std::sync::mpsc::channel::<String>();

        let received = Rc::new(Cell::new(false));
        let r = Rc::clone(&received);

        // Poll the channel from the main loop (simplified version of current pattern)
        glib::idle_add_local(move || {
            if let Ok(_msg) = rx.try_recv() {
                r.set(true);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });

        // Send from another thread (simulates Tokio → GTK)
        std::thread::spawn(move || {
            tx.send("from-tokio".to_string()).unwrap();
        })
        .join()
        .unwrap();

        flush_events();

        assert!(
            received.get(),
            "cross-thread send should deliver to main loop"
        );
    }

    /// Verifies that multiple events are dispatched independently.
    #[gtk::test]
    fn multiple_events_dispatched_in_order() {
        let events: Rc<std::cell::RefCell<Vec<String>>> =
            Rc::new(std::cell::RefCell::new(Vec::new()));

        let e1 = Rc::clone(&events);
        glib::idle_add_local_once(move || {
            e1.borrow_mut().push("first".to_string());
        });

        let e2 = Rc::clone(&events);
        glib::idle_add_local_once(move || {
            e2.borrow_mut().push("second".to_string());
        });

        let e3 = Rc::clone(&events);
        glib::idle_add_local_once(move || {
            e3.borrow_mut().push("third".to_string());
        });

        flush_events();

        let log = events.borrow();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0], "first");
        assert_eq!(log[1], "second");
        assert_eq!(log[2], "third");
    }
}

// ── GTK4 widget tests (need compositor) ─────────────────────────────────────

#[cfg(test)]
mod gtk_widgets {
    use super::*;

    #[gtk::test]
    fn button_label_roundtrip() {
        let button = gtk::Button::with_label("Delete");
        assert_eq!(button.label().unwrap(), "Delete");

        button.set_label("Restore");
        assert_eq!(button.label().unwrap(), "Restore");
    }

    #[gtk::test]
    fn box_packs_children_in_order() {
        let container = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let btn1 = gtk::Button::with_label("Favourite");
        let btn2 = gtk::Button::with_label("Trash");

        container.append(&btn1);
        container.append(&btn2);

        let first = container.first_child().expect("should have first child");
        assert!(first.eq(&btn1));

        let second = first.next_sibling().expect("should have second child");
        assert!(second.eq(&btn2));
    }

    #[gtk::test]
    fn multi_selection_tracks_selected_items() {
        let store = gtk::gio::ListStore::new::<glib::Object>();
        let selection = gtk::MultiSelection::new(Some(store.clone()));

        for _ in 0..5 {
            store.append(&glib::Object::new::<glib::Object>());
        }
        assert_eq!(selection.n_items(), 5);

        selection.select_item(0, false);
        selection.select_item(2, false);

        let bitset = selection.selection();
        assert_eq!(bitset.size(), 2);
    }

    #[gtk::test]
    fn widget_visibility_toggle() {
        // Simulates headerbar transformation on selection mode enter/exit
        let count_label = gtk::Label::new(Some("3 selected"));
        let zoom_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);

        // Normal mode
        count_label.set_visible(false);
        zoom_box.set_visible(true);
        assert!(!count_label.is_visible());
        assert!(zoom_box.is_visible());

        // Enter selection mode
        count_label.set_visible(true);
        zoom_box.set_visible(false);
        assert!(count_label.is_visible());
        assert!(!zoom_box.is_visible());

        // Exit selection mode
        count_label.set_visible(false);
        zoom_box.set_visible(true);
        assert!(!count_label.is_visible());
        assert!(zoom_box.is_visible());
    }

    #[gtk::test]
    fn check_button_active_state() {
        let checkbox = gtk::CheckButton::new();

        assert!(!checkbox.is_active());

        checkbox.set_active(true);
        assert!(checkbox.is_active());

        checkbox.set_active(false);
        assert!(!checkbox.is_active());
    }

    #[gtk::test]
    fn action_group_activation() {
        let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let group = gtk::gio::SimpleActionGroup::new();

        let entered = Rc::new(Cell::new(false));
        let e = Rc::clone(&entered);
        let action = gtk::gio::SimpleAction::new("enter-selection", None);
        action.connect_activate(move |_, _| {
            e.set(true);
        });
        group.add_action(&action);
        widget.insert_action_group("view", Some(&group));

        group.activate_action("enter-selection", None);
        flush_events();

        assert!(entered.get(), "action should have been activated");
    }

    #[gtk::test]
    fn stateful_action_tracks_boolean_state() {
        // Simulates selection-mode stateful action
        let action =
            gtk::gio::SimpleAction::new_stateful("selection-mode", None, &false.to_variant());

        assert!(!action.state().unwrap().get::<bool>().unwrap());

        action.change_state(&true.to_variant());
        assert!(action.state().unwrap().get::<bool>().unwrap());

        action.change_state(&false.to_variant());
        assert!(!action.state().unwrap().get::<bool>().unwrap());
    }
}

// ── libadwaita widget tests (need compositor) ───────────────────────────────

#[cfg(test)]
mod adw_widgets {
    use super::*;

    #[gtk::test]
    fn expander_row_title_and_subtitle() {
        adw::init().expect("adw::init failed");

        let row = adw::ExpanderRow::builder()
            .title("Transform")
            .subtitle("2 changes")
            .build();

        assert_eq!(row.title(), "Transform");
        assert_eq!(row.subtitle(), "2 changes");

        row.set_subtitle("Rotated 90");
        assert_eq!(row.subtitle(), "Rotated 90");
    }

    #[gtk::test]
    fn expander_row_expand_collapse() {
        adw::init().expect("adw::init failed");

        let row = adw::ExpanderRow::builder()
            .title("Filters")
            .expanded(false)
            .build();

        assert!(!row.is_expanded());
        row.set_expanded(true);
        assert!(row.is_expanded());
    }

    #[gtk::test]
    fn action_row_with_prefix_icon() {
        adw::init().expect("adw::init failed");

        let row = adw::ActionRow::builder().title("Location").build();

        let icon = gtk::Image::from_icon_name("find-location-symbolic");
        row.add_prefix(&icon);

        assert_eq!(row.title(), "Location");
        assert!(row.first_child().is_some());
    }

    #[gtk::test]
    fn toolbar_view_with_top_and_bottom_bars() {
        adw::init().expect("adw::init failed");

        let toolbar = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();
        let action_bar = gtk::ActionBar::new();

        toolbar.add_top_bar(&header);
        toolbar.add_bottom_bar(&action_bar);

        // Verify the bars are packed as children
        assert!(toolbar.first_child().is_some());
    }
}
