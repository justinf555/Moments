use std::collections::HashMap;
use std::rc::Rc;

use tracing::warn;

use super::ContentView;

/// Routes sidebar navigation to the correct content view with zero if/else dispatch.
///
/// Owns a `GtkStack` (the content pane) and a map from route id → `ContentView`.
/// Multiple route IDs can map to the same view and stack page via `register_alias`,
/// allowing a single `PhotoGridView` to serve both "photos" and "favorites".
pub struct ContentCoordinator {
    stack: gtk::Stack,
    views: HashMap<String, Rc<dyn ContentView>>,
    /// Maps a route id → the stack page name that holds its widget.
    page_for_route: HashMap<String, String>,
}

impl std::fmt::Debug for ContentCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContentCoordinator")
            .field("routes", &self.page_for_route.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ContentCoordinator {
    pub fn new(stack: gtk::Stack) -> Self {
        Self {
            stack,
            views: HashMap::new(),
            page_for_route: HashMap::new(),
        }
    }

    /// Register a view under the given route id.
    ///
    /// The view's root widget is added as a named child of the stack.
    pub fn register(&mut self, id: &str, view: Rc<dyn ContentView>) {
        self.stack.add_named(view.widget(), Some(id));
        self.page_for_route.insert(id.to_owned(), id.to_owned());
        self.views.insert(id.to_owned(), view);
    }

    /// Register an additional route id that reuses an existing view's stack page.
    ///
    /// `alias_id` is the new route name; `target_id` is the route whose view
    /// and stack page it shares. `on_navigate` receives `alias_id` so the view
    /// can distinguish between routes.
    pub fn register_alias(&mut self, alias_id: &str, target_id: &str) {
        if let Some(view) = self.views.get(target_id).cloned() {
            let page = self
                .page_for_route
                .get(target_id)
                .cloned()
                .unwrap_or_else(|| target_id.to_owned());
            self.page_for_route
                .insert(alias_id.to_owned(), page);
            self.views.insert(alias_id.to_owned(), view);
        } else {
            warn!(alias = %alias_id, target = %target_id, "register_alias: unknown target route");
        }
    }

    /// Switch the content pane to the view with the given route id.
    pub fn navigate(&self, id: &str) {
        if let (Some(view), Some(page)) = (self.views.get(id), self.page_for_route.get(id)) {
            self.stack.set_visible_child_name(page);
            view.on_navigate(id);
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
