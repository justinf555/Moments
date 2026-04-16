use std::cell::RefCell;
use std::sync::Arc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use tokio::sync::mpsc;
use tracing::{debug, error, instrument, warn};

use super::model::PersonItemObject;
use crate::library::faces::{FacesEvent, Person, PersonId};
use crate::library::Library;

/// Non-GObject dependencies for people operations.
struct PeopleDeps {
    library: Arc<Library>,
    tokio: tokio::runtime::Handle,
}

mod imp {
    use super::*;

    pub struct PeopleClientV2 {
        pub(super) deps: RefCell<Option<PeopleDeps>>,
        /// Weak references to all models created by this client.
        pub(super) models: RefCell<Vec<glib::WeakRef<gio::ListStore>>>,
    }

    impl Default for PeopleClientV2 {
        fn default() -> Self {
            Self {
                deps: RefCell::new(None),
                models: RefCell::new(Vec::new()),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PeopleClientV2 {
        const NAME: &'static str = "MomentsPeopleClientV2";
        type Type = super::PeopleClientV2;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for PeopleClientV2 {}
}

glib::wrapper! {
    /// GObject singleton that bridges the faces service to the GTK UI.
    ///
    /// Loads all people into unfiltered ListStore models and patches them
    /// in-place on mutations. Widgets apply their own filtering via
    /// `gtk::FilterListModel`. Tracks models via weak references so
    /// mutations propagate to all live views.
    ///
    /// Subscribes to `FacesEvent` from the service layer for reactive
    /// updates when the sync engine adds, updates, or removes people.
    pub struct PeopleClientV2(ObjectSubclass<imp::PeopleClientV2>);
}

impl Default for PeopleClientV2 {
    fn default() -> Self {
        Self::new()
    }
}

impl PeopleClientV2 {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Set the dependencies required for people operations and start
    /// listening for service events.
    ///
    /// Must be called once after construction, before any other method.
    pub fn configure(
        &self,
        library: Arc<Library>,
        tokio: tokio::runtime::Handle,
        events_rx: mpsc::UnboundedReceiver<FacesEvent>,
    ) {
        *self.imp().deps.borrow_mut() = Some(PeopleDeps {
            library: Arc::clone(&library),
            tokio: tokio.clone(),
        });

        let client_weak: glib::SendWeakRef<PeopleClientV2> = self.downgrade().into();
        tokio.spawn(Self::listen(events_rx, library, client_weak));
    }

    fn deps(&self) -> (Arc<Library>, tokio::runtime::Handle) {
        let deps = self.imp().deps.borrow();
        let deps = deps
            .as_ref()
            .expect("PeopleClientV2::configure() not called");
        (deps.library.clone(), deps.tokio.clone())
    }

    // ── Event listener ─────────────────────────────────────────────────

    /// Background task that receives `FacesEvent`s from the service and
    /// dispatches model patches on the GTK thread.
    async fn listen(
        mut rx: mpsc::UnboundedReceiver<FacesEvent>,
        library: Arc<Library>,
        client_weak: glib::SendWeakRef<PeopleClientV2>,
    ) {
        while let Some(event) = rx.recv().await {
            match event {
                FacesEvent::PersonAdded(id) => {
                    let person = library.faces().get_person(&id).await;
                    let thumb = library.faces().person_thumbnail_path(&id);
                    let weak = client_weak.clone();
                    glib::idle_add_once(move || {
                        if let Some(client) = weak.upgrade() {
                            match person {
                                Ok(Some(p)) => client.insert_into_models(&p, thumb),
                                Ok(None) => {
                                    warn!(person_id = %id, "person not found after add event")
                                }
                                Err(e) => {
                                    error!("failed to fetch added person: {e}");
                                    crate::client::show_error_toast(&e);
                                }
                            }
                        }
                    });
                }
                FacesEvent::PersonUpdated(id) => {
                    let person = library.faces().get_person(&id).await;
                    let weak = client_weak.clone();
                    glib::idle_add_once(move || {
                        if let Some(client) = weak.upgrade() {
                            match person {
                                Ok(Some(p)) => {
                                    client.update_in_models(id.as_str(), |item| {
                                        item.set_name(p.name.clone());
                                        item.set_is_hidden(p.is_hidden);
                                    });
                                }
                                Ok(None) => {
                                    warn!(person_id = %id, "person not found after update event")
                                }
                                Err(e) => {
                                    error!("failed to fetch updated person: {e}");
                                    crate::client::show_error_toast(&e);
                                }
                            }
                        }
                    });
                }
                FacesEvent::PersonRemoved(id) => {
                    let weak = client_weak.clone();
                    glib::idle_add_once(move || {
                        if let Some(client) = weak.upgrade() {
                            client.remove_from_models(id.as_str());
                        }
                    });
                }
            }
        }
        debug!("faces event listener shutting down");
    }

    // ── Factory ────────────────────────────────────────────────────────

    /// Create a new people list model. The client tracks it via weak ref
    /// and patches it in-place on mutations.
    ///
    /// Returns an unfiltered store containing all people. Widgets should
    /// wrap this in a `gtk::FilterListModel` to apply view-specific filters.
    pub fn create_model(&self) -> gio::ListStore {
        let store = gio::ListStore::new::<PersonItemObject>();
        self.imp().models.borrow_mut().push(store.downgrade());
        store
    }

    // ── Queries ─────────────────────────────────────────────────────────

    /// Fetch all people and splice into the given model.
    ///
    /// Loads every person from the service and replaces the model
    /// contents. Views apply their own filtering via `FilterListModel`.
    #[instrument(skip(self, model))]
    pub fn list_people(&self, model: &gio::ListStore) {
        let (library, tokio) = self.deps();
        let store = model.clone();

        glib::MainContext::default().spawn_local(async move {
            let lib = library.clone();
            let result =
                crate::client::spawn_on(&tokio, async move { lib.faces().list_people().await })
                    .await;

            match result {
                Ok(people) => {
                    let objects: Vec<glib::Object> = people
                        .iter()
                        .map(|person| {
                            let thumb = library.faces().person_thumbnail_path(&person.id);
                            PersonItemObject::new(person, thumb).upcast()
                        })
                        .collect();
                    store.splice(0, store.n_items(), &objects);
                    debug!(count = store.n_items(), "people loaded");
                }
                Err(e) => {
                    error!("failed to load people: {e}");
                    crate::client::show_error_toast(&e);
                }
            }
        });
    }

    // ── Mutations ──────────────────────────────────────────────────────

    /// Rename a person. On success, patches the name in all tracked models.
    #[instrument(skip(self))]
    pub fn rename_person(&self, id: PersonId, name: String) {
        let (library, tokio) = self.deps();
        let client_weak: glib::SendWeakRef<PeopleClientV2> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            let rename_id = id.clone();
            let n = name.clone();
            let result = crate::client::spawn_on(&tokio, async move {
                library.faces().rename_person(&rename_id, &n).await
            })
            .await;

            match result {
                Ok(()) => {
                    debug!(person_id = %id, name = %name, "person renamed");
                    if let Some(client) = client_weak.upgrade() {
                        client.update_in_models(id.as_str(), |item| {
                            item.set_name(name.clone());
                        });
                    }
                }
                Err(e) => {
                    error!("failed to rename person: {e}");
                    crate::client::show_error_toast(&e);
                }
            }
        });
    }

    /// Hide or unhide a person. On success, patches `is_hidden` in all
    /// tracked models. The widget's `FilterListModel` re-evaluates
    /// visibility automatically.
    #[instrument(skip(self))]
    pub fn set_person_hidden(&self, id: PersonId, hidden: bool) {
        let (library, tokio) = self.deps();
        let client_weak: glib::SendWeakRef<PeopleClientV2> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            let hide_id = id.clone();
            let result = crate::client::spawn_on(&tokio, async move {
                library.faces().set_person_hidden(&hide_id, hidden).await
            })
            .await;

            match result {
                Ok(()) => {
                    let action = if hidden { "hidden" } else { "unhidden" };
                    debug!(person_id = %id, action, "person visibility changed");
                    if let Some(client) = client_weak.upgrade() {
                        client.update_in_models(id.as_str(), |item| {
                            item.set_is_hidden(hidden);
                        });
                    }
                }
                Err(e) => {
                    error!("failed to set person visibility: {e}");
                    crate::client::show_error_toast(&e);
                }
            }
        });
    }

    // ── Model patching (private) ───────────────────────────────────────

    /// Insert a person into all tracked models. Prunes dead weak refs.
    fn insert_into_models(&self, person: &Person, thumb: Option<std::path::PathBuf>) {
        let mut models = self.imp().models.borrow_mut();
        let obj = PersonItemObject::new(person, thumb);
        models.retain(|weak| {
            if let Some(store) = weak.upgrade() {
                store.append(&obj);
                true
            } else {
                false
            }
        });
        debug!(person_id = %person.id, "insert_into_models");
    }

    /// Find a person by ID across all tracked models and apply an update.
    /// Prunes dead weak refs.
    fn update_in_models(&self, id: &str, update: impl Fn(&PersonItemObject)) {
        let mut models = self.imp().models.borrow_mut();
        let mut updated = 0u32;
        let mut live = 0u32;
        models.retain(|weak| {
            if let Some(store) = weak.upgrade() {
                live += 1;
                if let Some(item) = find_by_id(&store, id) {
                    update(&item);
                    updated += 1;
                }
                true
            } else {
                false
            }
        });
        debug!(
            person_id = id,
            live_models = live,
            updated_models = updated,
            "update_in_models"
        );
    }

    /// Remove a person by ID from all tracked models. Prunes dead weak refs.
    fn remove_from_models(&self, id: &str) {
        let mut models = self.imp().models.borrow_mut();
        models.retain(|weak| {
            let Some(store) = weak.upgrade() else {
                return false; // Dead ref, prune.
            };
            if let Some(item) = find_by_id(&store, id) {
                let pos = store.find(&item);
                if let Some(pos) = pos {
                    store.remove(pos);
                }
            }
            true
        });
        debug!(person_id = id, "remove_from_models");
    }
}

/// Find a `PersonItemObject` by person ID in a store.
fn find_by_id(store: &gio::ListStore, id: &str) -> Option<PersonItemObject> {
    for i in 0..store.n_items() {
        if let Some(obj) = store
            .item(i)
            .and_then(|o| o.downcast::<PersonItemObject>().ok())
        {
            if obj.id() == id {
                return Some(obj);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_person(id: &str, name: &str) -> Person {
        Person {
            id: PersonId::from_raw(id.to_string()),
            name: name.to_string(),
            face_count: 0,
            is_hidden: false,
        }
    }

    fn add_person(store: &gio::ListStore, person: Person) {
        store.append(&PersonItemObject::new(&person, None));
    }

    // ── find_by_id ────────────────────────────────────────────────────

    #[test]
    fn find_by_id_returns_matching_item() {
        let store = gio::ListStore::new::<PersonItemObject>();
        add_person(&store, test_person("p1", "Alice"));
        add_person(&store, test_person("p2", "Bob"));

        let item = find_by_id(&store, "p2").unwrap();
        assert_eq!(item.id(), "p2");
        assert_eq!(item.name(), "Bob");
    }

    #[test]
    fn find_by_id_returns_none_for_missing() {
        let store = gio::ListStore::new::<PersonItemObject>();
        add_person(&store, test_person("p1", "Alice"));

        assert!(find_by_id(&store, "missing").is_none());
    }

    #[test]
    fn find_by_id_empty_store() {
        let store = gio::ListStore::new::<PersonItemObject>();
        assert!(find_by_id(&store, "any").is_none());
    }

    // ── create_model ──────────────────────────────────────────────────

    #[test]
    fn create_model_tracks_weak_ref() {
        let client = PeopleClientV2::new();
        let store = client.create_model();

        assert_eq!(client.imp().models.borrow().len(), 1);
        assert!(client.imp().models.borrow()[0].upgrade().is_some());

        drop(store);
        assert!(client.imp().models.borrow()[0].upgrade().is_none());
    }

    // ── insert_into_models ────────────────────────────────────────────

    #[test]
    fn insert_into_models_adds_to_all_stores() {
        let client = PeopleClientV2::new();
        let store1 = client.create_model();
        let store2 = client.create_model();

        client.insert_into_models(&test_person("p1", "Alice"), None);

        assert_eq!(store1.n_items(), 1);
        assert_eq!(store2.n_items(), 1);
        let item: PersonItemObject = store1.item(0).unwrap().downcast().unwrap();
        assert_eq!(item.name(), "Alice");
    }

    #[test]
    fn insert_into_models_skips_dead_refs() {
        let client = PeopleClientV2::new();
        let live = client.create_model();
        let dead = client.create_model();
        drop(dead);

        client.insert_into_models(&test_person("p1", "Alice"), None);
        assert_eq!(live.n_items(), 1);
    }

    // ── update_in_models ──────────────────────────────────────────────

    #[test]
    fn update_in_models_patches_all_stores() {
        let client = PeopleClientV2::new();
        let store1 = client.create_model();
        let store2 = client.create_model();

        add_person(&store1, test_person("p1", "Old Name"));
        add_person(&store2, test_person("p1", "Old Name"));

        client.update_in_models("p1", |item| {
            item.set_name("New Name".to_string());
        });

        let item1: PersonItemObject = store1.item(0).unwrap().downcast().unwrap();
        let item2: PersonItemObject = store2.item(0).unwrap().downcast().unwrap();
        assert_eq!(item1.name(), "New Name");
        assert_eq!(item2.name(), "New Name");
    }

    #[test]
    fn update_in_models_no_match_is_noop() {
        let client = PeopleClientV2::new();
        let store = client.create_model();
        add_person(&store, test_person("p1", "Alice"));

        client.update_in_models("missing", |item| {
            item.set_name("Should Not Happen".to_string());
        });

        let item: PersonItemObject = store.item(0).unwrap().downcast().unwrap();
        assert_eq!(item.name(), "Alice");
    }

    #[test]
    fn update_in_models_skips_dead_refs() {
        let client = PeopleClientV2::new();
        let live = client.create_model();
        let dead = client.create_model();
        drop(dead);

        add_person(&live, test_person("p1", "Alice"));

        client.update_in_models("p1", |item| {
            item.set_name("Updated".to_string());
        });

        let item: PersonItemObject = live.item(0).unwrap().downcast().unwrap();
        assert_eq!(item.name(), "Updated");
    }

    #[test]
    fn update_in_models_patches_hidden_state() {
        let client = PeopleClientV2::new();
        let store = client.create_model();
        add_person(&store, test_person("p1", "Alice"));

        client.update_in_models("p1", |item| {
            item.set_is_hidden(true);
        });

        let item: PersonItemObject = store.item(0).unwrap().downcast().unwrap();
        assert!(item.is_hidden());
    }

    // ── remove_from_models ────────────────────────────────────────────

    #[test]
    fn remove_from_models_removes_from_all_stores() {
        let client = PeopleClientV2::new();
        let store1 = client.create_model();
        let store2 = client.create_model();

        add_person(&store1, test_person("p1", "Alice"));
        add_person(&store1, test_person("p2", "Bob"));
        add_person(&store2, test_person("p1", "Alice"));
        add_person(&store2, test_person("p2", "Bob"));

        client.remove_from_models("p1");

        assert_eq!(store1.n_items(), 1);
        assert_eq!(store2.n_items(), 1);
        let item: PersonItemObject = store1.item(0).unwrap().downcast().unwrap();
        assert_eq!(item.id(), "p2");
    }

    #[test]
    fn remove_from_models_prunes_dead_refs() {
        let client = PeopleClientV2::new();
        let _live = client.create_model();
        let dead = client.create_model();
        drop(dead);

        assert_eq!(client.imp().models.borrow().len(), 2);
        client.remove_from_models("any");
        assert_eq!(client.imp().models.borrow().len(), 1);
    }
}
