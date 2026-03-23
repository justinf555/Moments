/* window.rs
 *
 * Copyright 2026 Unknown
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

use std::rc::Rc;

use gtk::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use tracing::debug;

use crate::ui::photo_grid::{PhotoGrid, PhotoGridModel};

mod imp {
    use super::*;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/window.ui")]
    pub struct MomentsWindow {
        #[template_child]
        pub main_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub photo_grid: TemplateChild<PhotoGrid>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MomentsWindow {
        const NAME: &'static str = "MomentsWindow";
        type Type = super::MomentsWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            // Register MomentsPhotoGrid before binding the template so GTK
            // can resolve the type name used in window.ui.
            PhotoGrid::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MomentsWindow {}
    impl WidgetImpl for MomentsWindow {}
    impl WindowImpl for MomentsWindow {}
    impl ApplicationWindowImpl for MomentsWindow {}
    impl AdwApplicationWindowImpl for MomentsWindow {}
}

glib::wrapper! {
    pub struct MomentsWindow(ObjectSubclass<imp::MomentsWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow, adw::ApplicationWindow,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl MomentsWindow {
    pub fn new<P: IsA<gtk::Application>>(application: &P) -> Self {
        glib::Object::builder()
            .property("application", application)
            .build()
    }

    /// Attach a photo grid model once the library is ready.
    pub fn set_model(&self, model: Rc<PhotoGridModel>) {
        self.imp().photo_grid.set_model(model);
    }

    /// Switch from the loading page to the content page once the library is ready.
    pub fn set_library_ready(&self) {
        debug!("switching main window to content page");
        self.imp().main_stack.set_visible_child_name("content");
    }
}
