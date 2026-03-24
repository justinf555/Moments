use std::cell::{Cell, RefCell};

use gtk::{glib, prelude::*, subclass::prelude::*};

use super::item::MediaItemObject;

/// Handler IDs stored between `bind` and `unbind` calls.
///
/// Typed struct instead of unsafe `widget.set_data()` / `steal_data()`.
/// Disconnected explicitly in `unbind` so no signals fire on stale items.
pub struct CellBindings {
    item: glib::WeakRef<MediaItemObject>,
    texture_handler: glib::SignalHandlerId,
    favorite_handler: glib::SignalHandlerId,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PhotoGridCell {
        pub picture: gtk::Picture,
        pub spinner: gtk::Spinner,
        pub star_btn: gtk::Button,
        pub days_label: gtk::Label,
        pub duration_label: gtk::Label,
        pub overlay: gtk::Overlay,
        pub bindings: RefCell<Option<CellBindings>>,
        /// Whether to show the star button (false in Trash view).
        pub show_star: Cell<bool>,
        /// Whether the cell has a loaded texture (star hover only works when visible).
        pub has_texture: Cell<bool>,
        /// Whether the item is currently favourited (show star without hover).
        pub is_favorited: Cell<bool>,
        /// Click handler for the star button — connected in factory `bind`,
        /// disconnected in factory `unbind`.
        pub star_click_handler: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PhotoGridCell {
        const NAME: &'static str = "MomentsPhotoGridCell";
        type Type = super::PhotoGridCell;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("photo-grid-cell");
        }
    }

    impl ObjectImpl for PhotoGridCell {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            // Default cell size — overridden by the factory based on zoom level.
            obj.set_size_request(160, 160);

            self.picture.set_content_fit(gtk::ContentFit::Cover);
            self.picture.set_can_shrink(true);
            self.picture.set_visible(false);

            self.spinner.set_spinning(true);

            // Star button — bottom-left, shown on hover or when favourited.
            self.star_btn.set_icon_name("non-starred-symbolic");
            self.star_btn.set_halign(gtk::Align::Start);
            self.star_btn.set_valign(gtk::Align::End);
            self.star_btn.set_margin_start(4);
            self.star_btn.set_margin_bottom(4);
            self.star_btn.add_css_class("circular");
            self.star_btn.add_css_class("osd");
            self.star_btn.set_visible(false);

            // Trash days-remaining — bottom-right (Trash view only).
            self.days_label.set_halign(gtk::Align::End);
            self.days_label.set_valign(gtk::Align::End);
            self.days_label.set_margin_end(4);
            self.days_label.set_margin_bottom(4);
            self.days_label.add_css_class("osd");
            self.days_label.add_css_class("caption");
            self.days_label.add_css_class("pill");
            self.days_label.set_visible(false);

            // Video duration — bottom-right, always visible for videos.
            self.duration_label.set_halign(gtk::Align::End);
            self.duration_label.set_valign(gtk::Align::End);
            self.duration_label.set_margin_end(4);
            self.duration_label.set_margin_bottom(4);
            self.duration_label.add_css_class("osd");
            self.duration_label.add_css_class("caption");
            self.duration_label.add_css_class("pill");
            self.duration_label.set_visible(false);

            self.overlay.set_child(Some(&self.picture));
            self.overlay.add_overlay(&self.spinner);
            self.overlay.add_overlay(&self.star_btn);
            self.overlay.add_overlay(&self.days_label);
            self.overlay.add_overlay(&self.duration_label);

            // Hover controller — show star button on mouse enter/leave.
            let star = self.star_btn.clone();
            let show_star = self.show_star.clone();
            let has_texture = self.has_texture.clone();
            let is_favorited = self.is_favorited.clone();
            let motion = gtk::EventControllerMotion::new();
            motion.connect_enter(move |_, _, _| {
                if show_star.get() && has_texture.get() {
                    star.set_visible(true);
                }
            });
            let star = self.star_btn.clone();
            let is_favorited2 = self.is_favorited.clone();
            motion.connect_leave(move |_| {
                // Keep visible if item is favourited (visual indicator).
                if !is_favorited2.get() {
                    star.set_visible(false);
                }
            });
            self.overlay.add_controller(motion);

            self.overlay.set_parent(&*obj);
        }

        fn dispose(&self) {
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for PhotoGridCell {}
}

glib::wrapper! {
    pub struct PhotoGridCell(ObjectSubclass<imp::PhotoGridCell>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PhotoGridCell {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Connect to `item` and reflect its current texture and favourite state.
    pub fn bind(&self, item: &MediaItemObject) {
        self.update_from_item(item);
        self.update_star(item);
        self.update_days_remaining(item);
        self.update_duration(item);

        let cell = self.clone();
        let texture_handler = item.connect_texture_notify(move |item| {
            cell.update_from_item(item);
        });

        let cell = self.clone();
        let favorite_handler = item.connect_is_favorite_notify(move |item| {
            cell.update_star(item);
        });

        *self.imp().bindings.borrow_mut() = Some(CellBindings {
            item: item.downgrade(),
            texture_handler,
            favorite_handler,
        });
    }

    /// Disconnect signals and reset visual state.
    pub fn unbind(&self) {
        let imp = self.imp();
        if let Some(b) = imp.bindings.borrow_mut().take() {
            if let Some(item) = b.item.upgrade() {
                item.disconnect(b.texture_handler);
                item.disconnect(b.favorite_handler);
            }
        }
        imp.picture.set_paintable(None::<&gtk::gdk::Texture>);
        imp.picture.set_visible(false);
        imp.spinner.set_spinning(true);
        imp.spinner.set_visible(true);
        imp.star_btn.set_visible(false);
        imp.days_label.set_visible(false);
        imp.duration_label.set_visible(false);
        imp.has_texture.set(false);
        imp.is_favorited.set(false);
    }

    fn update_duration(&self, item: &MediaItemObject) {
        let imp = self.imp();
        let ms = item.duration_ms();
        if ms > 0 {
            imp.duration_label.set_text(&format!(" {} ", format_duration(ms)));
            imp.duration_label.set_visible(true);
        } else {
            imp.duration_label.set_visible(false);
        }
    }

    fn update_days_remaining(&self, item: &MediaItemObject) {
        let imp = self.imp();
        let trashed_at = item.trashed_at();
        if trashed_at > 0 {
            let now = chrono::Utc::now().timestamp();
            let elapsed_days = (now - trashed_at) / (24 * 60 * 60);
            let remaining = (30 - elapsed_days).max(0);
            let text = if remaining == 1 {
                " 1 day ".to_string()
            } else {
                format!(" {remaining} days ")
            };
            imp.days_label.set_text(&text);
            imp.days_label.set_visible(true);
        } else {
            imp.days_label.set_visible(false);
        }
    }

    fn update_star(&self, item: &MediaItemObject) {
        let imp = self.imp();
        let fav = item.is_favorite();
        imp.is_favorited.set(fav);
        if fav {
            imp.star_btn.set_icon_name("starred-symbolic");
            // Show starred indicator even without hover.
            if imp.show_star.get() && imp.has_texture.get() {
                imp.star_btn.set_visible(true);
            }
        } else {
            imp.star_btn.set_icon_name("non-starred-symbolic");
            // Hide unless hovering (hover controller handles visibility).
            imp.star_btn.set_visible(false);
        }
    }

    fn update_from_item(&self, item: &MediaItemObject) {
        let imp = self.imp();
        if let Some(texture) = item.texture() {
            imp.picture.set_paintable(Some(&texture));
            imp.picture.set_visible(true);
            imp.spinner.set_visible(false);
            imp.spinner.set_spinning(false);
            imp.has_texture.set(true);
            // Show star only if favourited (hover handles non-favourited).
            if imp.show_star.get() && imp.is_favorited.get() {
                imp.star_btn.set_visible(true);
            }
        } else {
            imp.picture.set_paintable(None::<&gtk::gdk::Texture>);
            imp.picture.set_visible(false);
            imp.spinner.set_visible(true);
            imp.spinner.set_spinning(true);
            imp.has_texture.set(false);
            imp.star_btn.set_visible(false);
        }
    }
}

/// Format a duration in milliseconds as `m:ss` or `h:mm:ss`.
fn format_duration(ms: u64) -> String {
    let total_secs = ms / 1000;
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_short() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(5_000), "0:05");
        assert_eq!(format_duration(65_000), "1:05");
        assert_eq!(format_duration(600_000), "10:00");
    }

    #[test]
    fn format_duration_long() {
        assert_eq!(format_duration(3_661_000), "1:01:01");
        assert_eq!(format_duration(7_200_000), "2:00:00");
    }
}
