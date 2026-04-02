//! Tests for the EventBus infrastructure (Phase 1 of #230).
//!
//! These tests validate the core fan-out subscriber pattern, cross-thread
//! delivery, and event filtering. They use #[gtk::test] because the bus
//! relies on glib::timeout_add_local for its drain source.

#![cfg(feature = "integration-tests")]

use std::cell::Cell;
use std::rc::Rc;

use gtk::glib;

use moments::app_event::AppEvent;
use moments::event_bus::EventBus;
use moments::library::media::MediaId;

/// Process GLib main loop events until the bus drain fires.
///
/// The EventBus uses a 4ms timeout source, so we need to wait at least
/// that long and then iterate the main loop to process the events.
fn flush_events() {
    // Sleep briefly to let the timeout source become ready
    std::thread::sleep(std::time::Duration::from_millis(10));
    // Drain all pending events
    while glib::MainContext::default().iteration(false) {}
}

// ── Fan-out delivery ────────────────────────────────────────────────────────

#[gtk::test]
fn single_subscriber_receives_event() {
    let bus = EventBus::new();

    let received = Rc::new(Cell::new(false));
    let r = Rc::clone(&received);
    bus.subscribe(move |event| {
        if matches!(event, AppEvent::SyncStarted) {
            r.set(true);
        }
    });

    bus.sender().send(AppEvent::SyncStarted).unwrap();
    flush_events();

    assert!(received.get());
}

#[gtk::test]
fn multiple_subscribers_all_receive() {
    let bus = EventBus::new();

    let count = Rc::new(Cell::new(0u32));
    for _ in 0..3 {
        let c = Rc::clone(&count);
        bus.subscribe(move |event| {
            if matches!(event, AppEvent::SyncStarted) {
                c.set(c.get() + 1);
            }
        });
    }

    bus.sender().send(AppEvent::SyncStarted).unwrap();
    flush_events();

    assert_eq!(count.get(), 3);
}

#[gtk::test]
fn subscribers_ignore_unmatched_events() {
    let bus = EventBus::new();

    let received = Rc::new(Cell::new(false));
    let r = Rc::clone(&received);
    bus.subscribe(move |event| {
        if matches!(event, AppEvent::SyncStarted) {
            r.set(true);
        }
    });

    // Send a different event
    bus.sender()
        .send(AppEvent::SyncComplete {
            assets: 0,
            people: 0,
            faces: 0,
            errors: 0,
        })
        .unwrap();
    flush_events();

    assert!(!received.get(), "subscriber should not fire for unmatched event");
}

#[gtk::test]
fn multiple_events_delivered_in_order() {
    let bus = EventBus::new();

    let log: Rc<std::cell::RefCell<Vec<String>>> = Rc::new(std::cell::RefCell::new(Vec::new()));
    let l = Rc::clone(&log);
    bus.subscribe(move |event| {
        let name = match event {
            AppEvent::SyncStarted => "start",
            AppEvent::SyncComplete { .. } => "complete",
            AppEvent::ThumbnailReady { .. } => "thumb",
            _ => return,
        };
        l.borrow_mut().push(name.to_string());
    });

    let tx = bus.sender();
    tx.send(AppEvent::SyncStarted).unwrap();
    tx.send(AppEvent::ThumbnailReady {
        media_id: MediaId::new("abc".to_string()),
    })
    .unwrap();
    tx.send(AppEvent::SyncComplete {
        assets: 10,
        people: 0,
        faces: 0,
        errors: 0,
    })
    .unwrap();
    flush_events();

    let entries = log.borrow();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0], "start");
    assert_eq!(entries[1], "thumb");
    assert_eq!(entries[2], "complete");
}

// ── Cross-thread delivery ───────────────────────────────────────────────────

#[gtk::test]
fn sender_works_from_another_thread() {
    let bus = EventBus::new();

    let received = Rc::new(Cell::new(false));
    let r = Rc::clone(&received);
    bus.subscribe(move |event| {
        if matches!(event, AppEvent::SyncStarted) {
            r.set(true);
        }
    });

    let tx = bus.sender();
    std::thread::spawn(move || {
        tx.send(AppEvent::SyncStarted).unwrap();
    })
    .join()
    .unwrap();

    flush_events();

    assert!(received.get(), "event from background thread should be delivered");
}

// ── Command / result event pattern ──────────────────────────────────────────

#[gtk::test]
fn command_event_reaches_subscriber() {
    let bus = EventBus::new();

    let trashed_ids: Rc<std::cell::RefCell<Vec<String>>> =
        Rc::new(std::cell::RefCell::new(Vec::new()));
    let t = Rc::clone(&trashed_ids);
    bus.subscribe(move |event| {
        if let AppEvent::TrashRequested { ids } = event {
            for id in ids {
                t.borrow_mut().push(id.as_str().to_string());
            }
        }
    });

    bus.sender()
        .send(AppEvent::TrashRequested {
            ids: vec![
                MediaId::new("a".to_string()),
                MediaId::new("b".to_string()),
            ],
        })
        .unwrap();
    flush_events();

    let ids = trashed_ids.borrow();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], "a");
    assert_eq!(ids[1], "b");
}

#[gtk::test]
fn result_event_reaches_subscriber() {
    let bus = EventBus::new();

    let favorite_state = Rc::new(Cell::new(false));
    let f = Rc::clone(&favorite_state);
    bus.subscribe(move |event| {
        if let AppEvent::FavoriteChanged { is_favorite, .. } = event {
            f.set(*is_favorite);
        }
    });

    bus.sender()
        .send(AppEvent::FavoriteChanged {
            ids: vec![MediaId::new("x".to_string())],
            is_favorite: true,
        })
        .unwrap();
    flush_events();

    assert!(favorite_state.get());
}
