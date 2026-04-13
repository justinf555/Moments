use std::collections::HashMap;

use gtk::prelude::*;
use tracing::{debug, warn};

/// A view slot that is either ready to display or waiting to be materialised.
enum ViewSlot {
    /// View widget is in the stack.
    Ready,
    /// View will be constructed on first [`ContentCoordinator::navigate`] call.
    /// The `Option` wrapper allows `take()` to move the `FnOnce` out.
    Lazy(Option<Box<dyn FnOnce() -> gtk::Widget>>),
}

/// Routes sidebar navigation to the correct content view.
///
/// Owns a `GtkStack` (the content pane) and a map from route id → [`ViewSlot`].
/// Eagerly registered views have their widget in the stack from the start.
/// Lazily registered views are materialised on first navigation — the factory
/// closure creates the view, its widget is added to the stack, and the slot
/// is replaced with `Ready`.
///
/// View-specific action groups are self-installed by each view on its own
/// widget — GTK's action resolution walks up the widget tree to find them.
///
/// See `docs/design-lazy-view-loading.md` for the full design rationale.
pub struct ContentCoordinator {
    stack: gtk::Stack,
    slots: HashMap<String, ViewSlot>,
}

impl std::fmt::Debug for ContentCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentCoordinator")
            .field("routes", &self.slots.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ContentCoordinator {
    pub fn new(stack: gtk::Stack) -> Self {
        Self {
            stack,
            slots: HashMap::new(),
        }
    }

    /// Register a view widget under the given route id (eager).
    ///
    /// The widget is added as a named child of the stack immediately.
    pub fn register(&mut self, id: &str, widget: &impl IsA<gtk::Widget>) {
        self.stack.add_named(widget, Some(id));
        self.slots.insert(id.to_owned(), ViewSlot::Ready);
    }

    /// Register a view factory that will be called on first navigation (lazy).
    ///
    /// No widget is added to the stack until [`navigate`](Self::navigate) is
    /// called with this route id. The factory closure should create the view
    /// widget, set up its model and event subscriptions, and return the widget.
    pub fn register_lazy<F>(&mut self, id: &str, factory: F)
    where
        F: FnOnce() -> gtk::Widget + 'static,
    {
        self.slots
            .insert(id.to_owned(), ViewSlot::Lazy(Some(Box::new(factory))));
    }

    /// Switch the content pane to the view with the given route id.
    ///
    /// If the slot is `Lazy`, the factory is called to materialise the view
    /// and add its widget to the stack. Subsequent navigations are instant.
    pub fn navigate(&mut self, id: &str) {
        let Some(slot) = self.slots.get_mut(id) else {
            warn!(route = %id, "navigate: unknown route");
            return;
        };

        // Materialise lazy views on first access.
        if let ViewSlot::Lazy(factory) = slot {
            let factory = factory.take().expect("lazy factory called once");
            debug!(route = %id, "materialising lazy view");
            let widget = factory();
            self.stack.add_named(&widget, Some(id));
            *slot = ViewSlot::Ready;
        }

        self.stack.set_visible_child_name(id);
    }

    /// Returns `true` if a route with the given id is registered.
    pub fn has_route(&self, id: &str) -> bool {
        self.slots.contains_key(id)
    }

    /// Remove a route and its widget from the stack.
    pub fn unregister(&mut self, id: &str) {
        if self.slots.remove(id).is_some() {
            if let Some(child) = self.stack.child_by_name(id) {
                self.stack.remove(&child);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // ContentCoordinator logic is pure Rust (HashMap + gtk::Stack calls).
    // Integration-level tests require a running GLib main loop and are
    // exercised via the Flatpak test workflow.
}
