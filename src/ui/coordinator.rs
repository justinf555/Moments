use std::collections::HashMap;
use std::rc::Rc;

use tracing::warn;

use super::ContentView;

/// Routes sidebar navigation to the correct content view.
///
/// Owns a `GtkStack` (the content pane) and a map from route id → `ContentView`.
/// Calling `navigate("photos")` looks up the view and switches the stack.
/// Each sidebar route has its own dedicated view instance.
pub struct ContentCoordinator {
    stack: gtk::Stack,
    views: HashMap<String, Rc<dyn ContentView>>,
}

impl std::fmt::Debug for ContentCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentCoordinator")
            .field("routes", &self.views.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ContentCoordinator {
    pub fn new(stack: gtk::Stack) -> Self {
        Self {
            stack,
            views: HashMap::new(),
        }
    }

    /// Register a view under the given route id.
    ///
    /// The view's root widget is added as a named child of the stack.
    pub fn register(&mut self, id: &str, view: Rc<dyn ContentView>) {
        self.stack.add_named(view.widget(), Some(id));
        self.views.insert(id.to_owned(), view);
    }

    /// Switch the content pane to the view with the given route id.
    pub fn navigate(&self, id: &str) {
        if self.views.contains_key(id) {
            self.stack.set_visible_child_name(id);
        } else {
            warn!(route = %id, "navigate: unknown route");
        }
    }
}

#[cfg(test)]
mod tests {
    // ContentCoordinator logic is pure Rust (HashMap + gtk::Stack calls).
    // Integration-level tests require a running GLib main loop and are
    // exercised via the Flatpak test workflow.
}
