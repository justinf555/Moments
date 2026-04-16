//! A container widget that adds right-click and long-press context menu support.
//!
//! Wraps a child widget and shows a `PopoverMenu` on right-click or long-press.
//! The menu model and action group are set by the owner and persist for the
//! widget's lifetime, avoiding the ephemeral action group issues with
//! standalone `PopoverMenu` construction.
//!
//! Inspired by Fractal's `ContextMenuBin` pattern.
//!
//! # Usage
//!
//! ```ignore
//! let bin = ContextMenuBin::new();
//! bin.set_child(Some(&my_card));
//! bin.set_context_menu(&menu_model, &action_group);
//! ```

use std::cell::RefCell;

use gtk::{gio, glib, prelude::*, subclass::prelude::*};
use tracing::debug;

mod imp {
    use super::*;
    use std::cell::Cell;

    pub struct ContextMenuBin {
        pub child: RefCell<Option<gtk::Widget>>,
        pub popover: RefCell<Option<gtk::PopoverMenu>>,
        pub action_group: RefCell<Option<gio::SimpleActionGroup>>,
        pub has_context_menu: Cell<bool>,
    }

    impl Default for ContextMenuBin {
        fn default() -> Self {
            Self {
                child: RefCell::new(None),
                popover: RefCell::new(None),
                action_group: RefCell::new(None),
                has_context_menu: Cell::new(false),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ContextMenuBin {
        const NAME: &'static str = "MomentsContextMenuBin";
        type Type = super::ContextMenuBin;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for ContextMenuBin {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            // Right-click gesture.
            let click = gtk::GestureClick::new();
            click.set_button(3);
            let weak = obj.downgrade();
            click.connect_pressed(move |gesture, _, x, y| {
                if let Some(bin) = weak.upgrade() {
                    bin.show_menu_at(x, y);
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
            });
            obj.add_controller(click);

            // Long-press gesture (touch support).
            let long_press = gtk::GestureLongPress::new();
            long_press.set_touch_only(true);
            let weak = obj.downgrade();
            long_press.connect_pressed(move |gesture, x, y| {
                if let Some(bin) = weak.upgrade() {
                    bin.show_menu_at(x, y);
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
            });
            obj.add_controller(long_press);
        }

        fn dispose(&self) {
            // Unparent the popover before the child.
            if let Some(popover) = self.popover.borrow_mut().take() {
                popover.unparent();
            }
            if let Some(child) = self.child.borrow_mut().take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for ContextMenuBin {}
}

glib::wrapper! {
    /// Container widget with built-in right-click / long-press context menu.
    pub struct ContextMenuBin(ObjectSubclass<imp::ContextMenuBin>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ContextMenuBin {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextMenuBin {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Set the child widget displayed inside the bin.
    pub fn set_child(&self, child: Option<&impl IsA<gtk::Widget>>) {
        let imp = self.imp();
        if let Some(old) = imp.child.borrow_mut().take() {
            old.unparent();
        }
        if let Some(child) = child {
            let widget = child.as_ref().clone();
            widget.set_parent(self);
            *imp.child.borrow_mut() = Some(widget);
        }
    }

    /// Get the child widget.
    pub fn child(&self) -> Option<gtk::Widget> {
        self.imp().child.borrow().clone()
    }

    /// Set the context menu model and action group.
    ///
    /// The action group is installed on this widget and persists until
    /// replaced or the widget is disposed. The `PopoverMenu` is created
    /// from the menu model and parented to this widget.
    pub fn set_context_menu(
        &self,
        menu_model: &gio::MenuModel,
        action_group: gio::SimpleActionGroup,
        action_prefix: &str,
    ) {
        let imp = self.imp();

        // Remove old popover.
        if let Some(old) = imp.popover.borrow_mut().take() {
            old.unparent();
        }

        // Install action group on this widget — persists for the widget's lifetime.
        self.insert_action_group(action_prefix, Some(&action_group));
        *imp.action_group.borrow_mut() = Some(action_group);

        // Create popover parented to this widget.
        let popover = gtk::PopoverMenu::from_model(Some(menu_model));
        popover.set_parent(self);
        popover.set_has_arrow(true);
        *imp.popover.borrow_mut() = Some(popover);

        imp.has_context_menu.set(true);
    }

    /// Clear the context menu.
    pub fn clear_context_menu(&self, action_prefix: &str) {
        let imp = self.imp();
        if let Some(popover) = imp.popover.borrow_mut().take() {
            popover.unparent();
        }
        self.insert_action_group(action_prefix, None::<&gio::SimpleActionGroup>);
        *imp.action_group.borrow_mut() = None;
        imp.has_context_menu.set(false);
    }

    /// Show the context menu at the given position.
    fn show_menu_at(&self, x: f64, y: f64) {
        let imp = self.imp();
        if !imp.has_context_menu.get() {
            return;
        }
        if let Some(popover) = imp.popover.borrow().as_ref() {
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                x as i32, y as i32, 1, 1,
            )));
            debug!("context menu opened at ({x}, {y})");
            popover.popup();
        }
    }
}

// Tests require a GTK display context (BinLayout) — use integration tests.
