use gettextrs::gettext;
use tracing::warn;

/// Describes a single entry in the sidebar navigation.
pub struct SidebarRoute {
    /// Internal page identifier — used as the `GtkStack` child name.
    pub id: &'static str,
    /// Symbolic icon name for the sidebar row.
    pub icon: &'static str,
}

impl SidebarRoute {
    /// Translated, human-readable label shown in the sidebar row.
    ///
    /// Labels are matched on `id` so the literals appear at this call
    /// site for `xgettext` extraction. Storing them on the struct
    /// hides the literal behind a variable and breaks extraction.
    pub fn label(&self) -> String {
        match self.id {
            "photos" => gettext("Photos"),
            "favorites" => gettext("Favorites"),
            "recent" => gettext("Recent Imports"),
            "people" => gettext("People"),
            "albums" => gettext("Albums"),
            "trash" => gettext("Trash"),
            other => {
                warn!(id = other, "SidebarRoute::label: no translation registered");
                other.to_string()
            }
        }
    }
}

/// All sidebar routes in display order.
///
/// Albums is a top-level destination (opens the Albums grid view).
/// Trash is promoted from the old "bottom routes" section to the
/// primary navigation alongside other system destinations.
pub const ROUTES: &[SidebarRoute] = &[
    SidebarRoute {
        id: "photos",
        icon: "image-x-generic-symbolic",
    },
    SidebarRoute {
        id: "favorites",
        icon: "starred-symbolic",
    },
    SidebarRoute {
        id: "recent",
        icon: "document-open-recent-symbolic",
    },
    SidebarRoute {
        id: "people",
        icon: "system-users-symbolic",
    },
    SidebarRoute {
        id: "albums",
        icon: "folder-symbolic",
    },
    SidebarRoute {
        id: "trash",
        icon: "user-trash-symbolic",
    },
];
