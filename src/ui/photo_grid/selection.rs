//! App-owned selection state for the photo grid.
//!
//! The grid uses [`gtk::NoSelection`] as its `GridView` selection model
//! (#547). Selection is tracked here, keyed on [`MediaId`] rather than
//! position, so virtualization-driven recycling does not corrupt the
//! state. Cells subscribe to the `changed` signal during bind to keep
//! their checkbox in sync.

use std::cell::RefCell;
use std::collections::HashSet;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::library::media::MediaId;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SelectionState {
        pub set: RefCell<HashSet<MediaId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SelectionState {
        const NAME: &'static str = "MomentsPhotoGridSelectionState";
        type Type = super::SelectionState;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for SelectionState {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<glib::subclass::Signal>> =
                std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| vec![glib::subclass::Signal::builder("changed").build()])
        }
    }
}

glib::wrapper! {
    /// Selection set for one photo grid view.
    ///
    /// Each [`PhotoGridView`] owns a single instance. Mutations emit a
    /// `changed` signal so visible cells can update their checkbox
    /// state without polling.
    ///
    /// [`PhotoGridView`]: super::PhotoGridView
    pub struct SelectionState(ObjectSubclass<imp::SelectionState>);
}

impl Default for SelectionState {
    fn default() -> Self {
        Self::new()
    }
}

impl SelectionState {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Insert `id`. Returns `true` if it was newly added; emits `changed`
    /// only when the state actually moved.
    pub fn insert(&self, id: MediaId) -> bool {
        let inserted = self.imp().set.borrow_mut().insert(id);
        if inserted {
            self.emit_by_name::<()>("changed", &[]);
        }
        inserted
    }

    /// Remove `id`. Returns `true` if it was present; emits `changed`
    /// only when the state actually moved.
    pub fn remove(&self, id: &MediaId) -> bool {
        let removed = self.imp().set.borrow_mut().remove(id);
        if removed {
            self.emit_by_name::<()>("changed", &[]);
        }
        removed
    }

    /// Drop every selected id. Emits `changed` only if the set was
    /// non-empty.
    pub fn clear(&self) {
        let was_nonempty = !self.imp().set.borrow().is_empty();
        if was_nonempty {
            self.imp().set.borrow_mut().clear();
            self.emit_by_name::<()>("changed", &[]);
        }
    }

    pub fn contains(&self, id: &MediaId) -> bool {
        self.imp().set.borrow().contains(id)
    }

    pub fn is_empty(&self) -> bool {
        self.imp().set.borrow().is_empty()
    }

    pub fn len(&self) -> usize {
        self.imp().set.borrow().len()
    }

    /// Snapshot of the selected ids. Allocates; cheap for selection-mode
    /// workloads (tens to hundreds of items).
    pub fn ids(&self) -> Vec<MediaId> {
        self.imp().set.borrow().iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    fn id(s: &str) -> MediaId {
        MediaId::new(format!("{s:0<32}"))
    }

    fn count_changes(state: &SelectionState) -> Rc<Cell<u32>> {
        let counter = Rc::new(Cell::new(0u32));
        let c = Rc::clone(&counter);
        state.connect_closure(
            "changed",
            false,
            glib::closure_local!(move |_: SelectionState| {
                c.set(c.get() + 1);
            }),
        );
        counter
    }

    #[test]
    fn new_is_empty() {
        let s = SelectionState::new();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert!(s.ids().is_empty());
    }

    #[test]
    fn insert_returns_true_then_false() {
        let s = SelectionState::new();
        let a = id("a");
        assert!(s.insert(a.clone()));
        assert!(!s.insert(a));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn remove_returns_true_on_hit_false_on_miss() {
        let s = SelectionState::new();
        let a = id("a");
        s.insert(a.clone());
        assert!(s.remove(&a));
        assert!(!s.remove(&a));
        assert!(s.is_empty());
    }

    #[test]
    fn clear_empties() {
        let s = SelectionState::new();
        s.insert(id("a"));
        s.insert(id("b"));
        s.clear();
        assert!(s.is_empty());
    }

    #[test]
    fn contains_reflects_membership() {
        let s = SelectionState::new();
        let a = id("a");
        let b = id("b");
        s.insert(a.clone());
        assert!(s.contains(&a));
        assert!(!s.contains(&b));
    }

    #[test]
    fn ids_returns_all() {
        let s = SelectionState::new();
        s.insert(id("a"));
        s.insert(id("b"));
        s.insert(id("c"));
        let mut ids = s.ids();
        ids.sort_by(|x, y| x.as_str().cmp(y.as_str()));
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn changed_fires_only_on_real_mutations() {
        let s = SelectionState::new();
        let counter = count_changes(&s);
        let a = id("a");

        s.insert(a.clone()); // 1
        s.insert(a.clone()); // no-op
        s.remove(&a); // 2
        s.remove(&a); // no-op
        s.clear(); // already empty: no-op
        s.insert(id("b")); // 3
        s.clear(); // 4

        assert_eq!(counter.get(), 4);
    }
}
