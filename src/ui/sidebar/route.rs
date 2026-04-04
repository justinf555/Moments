/// Describes a single entry in the sidebar navigation.
pub struct SidebarRoute {
    /// Internal page identifier — used as the `GtkStack` child name.
    pub id: &'static str,
    /// Human-readable label shown in the sidebar row.
    pub label: &'static str,
    /// Symbolic icon name for the sidebar row.
    pub icon: &'static str,
}

/// All sidebar routes in display order.
///
/// Albums is a top-level destination (opens the Albums grid view).
/// Trash is promoted from the old "bottom routes" section to the
/// primary navigation alongside other system destinations.
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
        id: "people",
        label: "People",
        icon: "system-users-symbolic",
    },
    SidebarRoute {
        id: "albums",
        label: "Albums",
        icon: "folder-symbolic",
    },
    SidebarRoute {
        id: "trash",
        label: "Trash",
        icon: "user-trash-symbolic",
    },
];

/// Index of the Trash item in [`ROUTES`] for direct access (badge updates).
pub const TRASH_INDEX: u32 = 5;
