/// Describes a single entry in the sidebar navigation.
pub struct SidebarRoute {
    /// Internal page identifier — used as the `GtkStack` child name.
    pub id: &'static str,
    /// Human-readable label shown in the sidebar row.
    pub label: &'static str,
    /// Symbolic icon name for the sidebar row.
    pub icon: &'static str,
}

/// All sidebar routes, in display order.
///
/// Adding a new view is one entry here — no widget code changes required.
pub const ROUTES: &[SidebarRoute] = &[
    SidebarRoute {
        id: "photos",
        label: "Photos",
        icon: "image-x-generic-symbolic",
    },
    SidebarRoute {
        id: "favorites",
        label: "Favorites",
        icon: "starred-symbolic",
    },
    SidebarRoute {
        id: "recent",
        label: "Recent Imports",
        icon: "document-open-recent-symbolic",
    },
    SidebarRoute {
        id: "trash",
        label: "Trash",
        icon: "user-trash-symbolic",
    },
];
