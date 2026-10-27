// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared helpers for unit tests that construct GObjects.

use std::sync::Once;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};

use super::{
    AlbumClientV2, AlbumItemObject, ImportClient, MediaClientV2, MediaItemObject, PeopleClientV2,
    PersonItemObject, SyncClient,
};

static INIT: Once = Once::new();

fn register<T: StaticType>() {
    // Take a class ref so `class_init` (which may register further foreign
    // types as property types) runs here, serialised by the `Once`.
    let _ = glib::object::Class::<glib::Object>::from_type(T::static_type());
}

/// Register every GObject type the client models use, exactly once.
///
/// Cargo runs tests on parallel threads with no GTK main loop. If two threads
/// first-use `gdk::Texture` and a Moments model type at the same time, GLib's
/// type-system lock and the Texture type's init-once are taken in opposite
/// orders and the process deadlocks (#728). Registering everything up front
/// under a `Once` removes the race. The app never hits this because all
/// GObjects are created on the GTK main thread.
///
/// Call this first in any unit test that constructs a GObject, a
/// `gio::ListStore` or a texture.
pub(crate) fn ensure_gobject_types() {
    INIT.call_once(|| {
        register::<gdk::Texture>();
        register::<gdk::MemoryTexture>();
        register::<gio::ListStore>();
        register::<AlbumItemObject>();
        register::<PersonItemObject>();
        register::<MediaItemObject>();
        register::<AlbumClientV2>();
        register::<PeopleClientV2>();
        register::<MediaClientV2>();
        register::<SyncClient>();
        register::<ImportClient>();
    });
}
