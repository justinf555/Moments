//! Tests for the EventBus infrastructure (#230).
//!
//! These tests validate the core fan-out subscriber pattern, cross-thread
//! delivery, and event filtering.

#![cfg(feature = "integration-tests")]

use std::cell::Cell;
use std::rc::Rc;

use gtk::glib;

use moments::app_event::AppEvent;
use moments::event_bus::EventBus;
use moments::library::media::MediaId;

/// Process all pending GLib main loop events.
fn flush_events() {
    while glib::MainContext::default().iteration(false) {}
}

// ── Fan-out delivery ────────────────────────────────────────────────────────

#[gtk::test]
fn single_subscriber_receives_event() {
    let bus = EventBus::new();

    let received = Rc::new(Cell::new(false));
    let r = Rc::clone(&received);
    let _sub = bus.subscribe(move |event| {
        if matches!(event, AppEvent::SyncStarted) {
            r.set(true);
        }
    });

    bus.sender().send(AppEvent::SyncStarted);
    flush_events();

    assert!(received.get());
}

#[gtk::test]
fn multiple_subscribers_all_receive() {
    let bus = EventBus::new();

    let count = Rc::new(Cell::new(0u32));
    let mut subs = Vec::new();
    for _ in 0..3 {
        let c = Rc::clone(&count);
        subs.push(bus.subscribe(move |event| {
            if matches!(event, AppEvent::SyncStarted) {
                c.set(c.get() + 1);
            }
        }));
    }

    bus.sender().send(AppEvent::SyncStarted);
    flush_events();

    assert_eq!(count.get(), 3);
}

#[gtk::test]
fn subscribers_ignore_unmatched_events() {
    let bus = EventBus::new();

    let received = Rc::new(Cell::new(false));
    let r = Rc::clone(&received);
    let _sub = bus.subscribe(move |event| {
        if matches!(event, AppEvent::SyncStarted) {
            r.set(true);
        }
    });

    bus.sender().send(AppEvent::SyncComplete {
        assets: 0,
        people: 0,
        faces: 0,
        errors: 0,
    });
    flush_events();

    assert!(
        !received.get(),
        "subscriber should not fire for unmatched event"
    );
}

#[gtk::test]
fn multiple_events_delivered_in_order() {
    let bus = EventBus::new();

    let log: Rc<std::cell::RefCell<Vec<String>>> = Rc::new(std::cell::RefCell::new(Vec::new()));
    let l = Rc::clone(&log);
    let _sub = bus.subscribe(move |event| {
        let name = match event {
            AppEvent::SyncStarted => "start",
            AppEvent::SyncComplete { .. } => "complete",
            AppEvent::ThumbnailReady { .. } => "thumb",
            _ => return,
        };
        l.borrow_mut().push(name.to_string());
    });

    let tx = bus.sender();
    tx.send(AppEvent::SyncStarted);
    tx.send(AppEvent::ThumbnailReady {
        media_id: MediaId::new("abc".to_string()),
    });
    tx.send(AppEvent::SyncComplete {
        assets: 10,
        people: 0,
        faces: 0,
        errors: 0,
    });
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
    let _sub = bus.subscribe(move |event| {
        if matches!(event, AppEvent::SyncStarted) {
            r.set(true);
        }
    });

    let tx = bus.sender();
    std::thread::spawn(move || {
        tx.send(AppEvent::SyncStarted);
    })
    .join()
    .unwrap();

    flush_events();

    assert!(
        received.get(),
        "event from background thread should be delivered"
    );
}

// ── Command / result event pattern ──────────────────────────────────────────

#[gtk::test]
fn command_event_reaches_subscriber() {
    let bus = EventBus::new();

    let trashed_ids: Rc<std::cell::RefCell<Vec<String>>> =
        Rc::new(std::cell::RefCell::new(Vec::new()));
    let t = Rc::clone(&trashed_ids);
    let _sub = bus.subscribe(move |event| {
        if let AppEvent::TrashRequested { ids } = event {
            for id in ids {
                t.borrow_mut().push(id.as_str().to_string());
            }
        }
    });

    bus.sender().send(AppEvent::TrashRequested {
        ids: vec![MediaId::new("a".to_string()), MediaId::new("b".to_string())],
    });
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
    let _sub = bus.subscribe(move |event| {
        if let AppEvent::FavoriteChanged { is_favorite, .. } = event {
            f.set(*is_favorite);
        }
    });

    bus.sender().send(AppEvent::FavoriteChanged {
        ids: vec![MediaId::new("x".to_string())],
        is_favorite: true,
    });
    flush_events();

    assert!(favorite_state.get());
}

// ── Drop cleanup ────────────────────────────────────────────────────────────

#[gtk::test]
fn drop_cleans_up_thread_local_state() {
    {
        let bus = EventBus::new();
        let _sub = bus.subscribe(|_| {});
        // bus dropped here
    }

    // Creating a new bus should work (thread-local state was cleared)
    let bus = EventBus::new();
    let received = Rc::new(Cell::new(false));
    let r = Rc::clone(&received);
    let _sub = bus.subscribe(move |event| {
        if matches!(event, AppEvent::SyncStarted) {
            r.set(true);
        }
    });

    bus.sender().send(AppEvent::SyncStarted);
    flush_events();

    assert!(received.get(), "new bus after drop should work");
}

// ── Re-entrancy safety ─────────────────────────────────────────────────────

#[gtk::test]
fn dropping_subscription_during_dispatch_does_not_panic() {
    let bus = EventBus::new();

    // Subscriber A holds a subscription for subscriber B.
    // When A handles an event it drops B's subscription — this must not panic.
    let sub_b: Rc<std::cell::RefCell<Option<moments::event_bus::Subscription>>> =
        Rc::new(std::cell::RefCell::new(None));

    let b_count = Rc::new(Cell::new(0u32));
    let bc = Rc::clone(&b_count);
    let sub = bus.subscribe(move |event| {
        if matches!(event, AppEvent::SyncStarted) {
            bc.set(bc.get() + 1);
        }
    });
    *sub_b.borrow_mut() = Some(sub);

    let sb = Rc::clone(&sub_b);
    let _sub_a = bus.subscribe(move |event| {
        if matches!(event, AppEvent::SyncStarted) {
            // Drop subscriber B's subscription from within dispatch.
            sb.borrow_mut().take();
        }
    });

    // First event: A drops B's subscription during dispatch.
    // B still fires because the SUBSCRIBERS immutable borrow is held for the entire
    // dispatch cycle — the removal is deferred until after the loop completes.
    bus.sender().send(AppEvent::SyncStarted);
    flush_events();
    assert_eq!(
        b_count.get(),
        1,
        "B fires during the dispatch cycle it was dropped"
    );

    // Second event: B should no longer fire — it was removed after the first cycle.
    bus.sender().send(AppEvent::SyncStarted);
    flush_events();
    assert_eq!(b_count.get(), 1, "B should not fire after being dropped");
}

// ── Subscription unsubscribe ────────────────────────────────────────────────

#[gtk::test]
fn dropping_subscription_removes_subscriber() {
    let bus = EventBus::new();

    let count = Rc::new(Cell::new(0u32));
    let c = Rc::clone(&count);
    let sub = bus.subscribe(move |event| {
        if matches!(event, AppEvent::SyncStarted) {
            c.set(c.get() + 1);
        }
    });

    bus.sender().send(AppEvent::SyncStarted);
    flush_events();
    assert_eq!(count.get(), 1);

    // Drop the subscription — subscriber should be removed.
    drop(sub);

    bus.sender().send(AppEvent::SyncStarted);
    flush_events();
    assert_eq!(count.get(), 1, "subscriber should not fire after drop");
}
