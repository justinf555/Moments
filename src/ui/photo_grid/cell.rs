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
        pub placeholder: gtk::Image,
        pub star_btn: gtk::Button,
        pub checkbox: gtk::CheckButton,
        pub days_label: gtk::Label,
        pub duration_label: gtk::Label,
        pub overlay: gtk::Overlay,
        pub bindings: RefCell<Option<CellBindings>>,
        /// Whether to show the star button (false in Trash view).
        pub show_star: Cell<bool>,
        /// Whether the cell has a loaded texture.
        pub has_texture: Cell<bool>,
        /// Whether the item is currently favourited.
        pub is_favorited: Cell<bool>,
        /// Whether the grid is in selection mode (checkbox always visible).
        pub in_selection_mode: Cell<bool>,
        /// Click handler for the star button — connected in factory `bind`,
        /// disconnected in factory `unbind`.
        pub star_click_handler: RefCell<Option<glib::SignalHandlerId>>,
        /// Click handler for the checkbox — connected in factory `bind`,
        /// disconnected in factory `unbind`.
        pub checkbox_handler: RefCell<Option<glib::SignalHandlerId>>,
        /// Debounce timer for thumbnail loading — cancelled on unbind so
        /// fast-scrolled cells never decode textures.
        pub texture_timer: RefCell<Option<glib::SourceId>>,
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

            // Static placeholder shown while thumbnail loads — no animation,
            // zero CPU cost (replaces GtkSpinner which caused scroll jank).
            self.placeholder.set_icon_name(Some("image-x-generic-symbolic"));
            self.placeholder.set_pixel_size(48);
            self.placeholder.set_halign(gtk::Align::Center);
            self.placeholder.set_valign(gtk::Align::Center);
            self.placeholder.add_css_class("dim-label");

            // Star button — bottom-left, only shown when favourited (no hover).
            self.star_btn.set_icon_name("non-starred-symbolic");
            self.star_btn.set_halign(gtk::Align::Start);
            self.star_btn.set_valign(gtk::Align::End);
            self.star_btn.set_margin_start(4);
            self.star_btn.set_margin_bottom(4);
            self.star_btn.add_css_class("circular");
            self.star_btn.add_css_class("osd");
            self.star_btn.set_visible(false);

            // Checkbox — top-left, shown on hover or always in selection mode.
            self.checkbox.set_halign(gtk::Align::Start);
            self.checkbox.set_valign(gtk::Align::Start);
            self.checkbox.set_margin_start(6);
            self.checkbox.set_margin_top(6);
            self.checkbox.add_css_class("selection-mode");
            self.checkbox.add_css_class("osd");
            self.checkbox.set_visible(false);

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
            self.overlay.add_overlay(&self.placeholder);
            self.overlay.add_overlay(&self.star_btn);
            self.overlay.add_overlay(&self.checkbox);
            self.overlay.add_overlay(&self.days_label);
            self.overlay.add_overlay(&self.duration_label);

            self.overlay.set_parent(&*obj);

            // Hover controller — show checkbox on mouse enter/leave.
            let motion = gtk::EventControllerMotion::new();
            motion.set_propagation_phase(gtk::PropagationPhase::Capture);
            let cell_weak = obj.downgrade();
            motion.connect_enter(move |_, _x, _y| {
                let Some(cell) = cell_weak.upgrade() else { return };
                let imp = cell.imp();
                if imp.has_texture.get() {
                    imp.checkbox.set_visible(true);
                }
            });
            let cell_weak = obj.downgrade();
            motion.connect_leave(move |_| {
                let Some(cell) = cell_weak.upgrade() else { return };
                let imp = cell.imp();
                // Only hide checkbox if NOT in selection mode.
                if !imp.in_selection_mode.get() {
                    imp.checkbox.set_visible(false);
                }
            });
            obj.add_controller(motion);
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
        imp.placeholder.set_visible(true);
        imp.star_btn.set_visible(false);
        imp.checkbox.set_visible(false);
        imp.checkbox.set_active(false);
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
            let retention_days = gtk::gio::SettingsSchemaSource::default()
                .and_then(|src| src.lookup("io.github.justinf555.Moments", true))
                .map(|_| {
                    gtk::gio::Settings::new("io.github.justinf555.Moments")
                        .uint("trash-retention-days") as i64
                })
                .unwrap_or(30);
            let now = chrono::Utc::now().timestamp();
            let elapsed_days = (now - trashed_at) / (24 * 60 * 60);
            let remaining = (retention_days - elapsed_days).max(0);
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

    /// Set whether the cell is in selection mode (checkbox always visible).
    pub fn set_selection_mode(&self, active: bool) {
        let imp = self.imp();
        imp.in_selection_mode.set(active);
        if active && imp.has_texture.get() {
            imp.checkbox.set_visible(true);
        } else if !active {
            imp.checkbox.set_visible(false);
            imp.checkbox.set_active(false);
        }
    }

    /// Set the checkbox checked state (reflects MultiSelection).
    pub fn set_checked(&self, checked: bool) {
        self.imp().checkbox.set_active(checked);
    }

    fn update_star(&self, item: &MediaItemObject) {
        let imp = self.imp();
        let fav = item.is_favorite();
        imp.is_favorited.set(fav);
        if fav && imp.show_star.get() && imp.has_texture.get() {
            imp.star_btn.set_icon_name("starred-symbolic");
            imp.star_btn.set_visible(true);
        } else {
            imp.star_btn.set_icon_name("non-starred-symbolic");
            imp.star_btn.set_visible(false);
        }
    }

    fn update_from_item(&self, item: &MediaItemObject) {
        let imp = self.imp();
        if let Some(texture) = item.texture() {
            imp.picture.set_paintable(Some(&texture));
            imp.picture.set_visible(true);
            imp.placeholder.set_visible(false);
            imp.has_texture.set(true);
            // Show star only if favourited (hover handles non-favourited).
            if imp.show_star.get() && imp.is_favorited.get() {
                imp.star_btn.set_visible(true);
            }
        } else {
            imp.picture.set_paintable(None::<&gtk::gdk::Texture>);
            imp.picture.set_visible(false);
            imp.placeholder.set_visible(true);
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
