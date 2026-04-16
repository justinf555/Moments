use std::cell::{Cell, RefCell};
use std::sync::Arc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use tracing::{error, info};

use super::model::PersonItemObject;
use crate::library::faces::PersonId;
use crate::library::Library;

/// Filter parameters associated with a tracked model.
struct TrackedModel {
    store: glib::WeakRef<gio::ListStore>,
    include_hidden: Cell<bool>,
    include_unnamed: Cell<bool>,
}

/// Non-GObject dependencies for people operations.
struct PeopleDeps {
    library: Arc<Library>,
    tokio: tokio::runtime::Handle,
}

mod imp {
    use super::*;

    pub struct PeopleClient {
        pub(super) deps: RefCell<Option<PeopleDeps>>,
        pub(super) models: RefCell<Vec<TrackedModel>>,
    }

    impl Default for PeopleClient {
        fn default() -> Self {
            Self {
                deps: RefCell::new(None),
                models: RefCell::new(Vec::new()),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PeopleClient {
        const NAME: &'static str = "MomentsPeopleClient";
        type Type = super::PeopleClient;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for PeopleClient {}
}

glib::wrapper! {
    /// GObject singleton that bridges the faces service to the GTK UI.
    ///
    /// Acts as a factory for people list models. Tracks models with their
    /// filter state and patches them in-place on mutations (rename, hide/unhide).
    pub struct PeopleClient(ObjectSubclass<imp::PeopleClient>);
}

impl Default for PeopleClient {
    fn default() -> Self {
        Self::new()
    }
}

impl PeopleClient {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Set the dependencies required for people operations.
    pub fn configure(&self, library: Arc<Library>, tokio: tokio::runtime::Handle) {
        *self.imp().deps.borrow_mut() = Some(PeopleDeps { library, tokio });
    }

    fn deps(&self) -> (Arc<Library>, tokio::runtime::Handle) {
        let deps = self.imp().deps.borrow();
        let deps = deps.as_ref().expect("PeopleClient::configure() not called");
        (deps.library.clone(), deps.tokio.clone())
    }

    // ── Factory ────────────────────────────────────────────────────────

    /// Create a new people list model. The client tracks it with the given
    /// filter state and patches it in-place on mutations.
    pub fn create_model(&self, include_hidden: bool, include_unnamed: bool) -> gio::ListStore {
        let store = gio::ListStore::new::<PersonItemObject>();
        self.imp().models.borrow_mut().push(TrackedModel {
            store: store.downgrade(),
            include_hidden: Cell::new(include_hidden),
            include_unnamed: Cell::new(include_unnamed),
        });
        store
    }

    /// Populate a model with people from the service.
    ///
    /// Updates the stored filter state for this model, then fetches and
    /// splices results into the store on the GTK thread.
    pub fn populate(&self, model: &gio::ListStore, include_hidden: bool, include_unnamed: bool) {
        // Update stored filter state.
        {
            let models = self.imp().models.borrow();
            for tracked in models.iter() {
                if let Some(store) = tracked.store.upgrade() {
                    if store == *model {
                        tracked.include_hidden.set(include_hidden);
                        tracked.include_unnamed.set(include_unnamed);
                    }
                }
            }
        }

        let (library, tokio) = self.deps();
        let store = model.clone();

        glib::MainContext::default().spawn_local(async move {
            let lib = library.clone();
            let result = tokio
                .spawn(async move {
                    lib.faces()
                        .list_people(include_hidden, include_unnamed)
                        .await
                })
                .await;

            match result {
                Ok(Ok(people)) => {
                    info!(
                        count = people.len(),
                        include_hidden, include_unnamed, "people populated"
                    );
                    let objects: Vec<glib::Object> = people
                        .iter()
                        .map(|person| {
                            let thumb = library.faces().person_thumbnail_path(&person.id);
                            PersonItemObject::new(person, thumb).upcast()
                        })
                        .collect();
                    store.splice(0, store.n_items(), &objects);
                }
                Ok(Err(e)) => error!("failed to load people: {e}"),
                Err(e) => error!("tokio join error loading people: {e}"),
            }
        });
    }

    // ── Mutations ──────────────────────────────────────────────────────

    /// Rename a person. On success, patches the name in all tracked models.
    pub fn rename_person(
        &self,
        id: PersonId,
        name: String,
        on_error: impl FnOnce(String) + 'static,
    ) {
        let (library, tokio) = self.deps();
        let client_weak: glib::SendWeakRef<PeopleClient> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            let rename_id = id.clone();
            let n = name.clone();
            let result = tokio
                .spawn(async move { library.faces().rename_person(&rename_id, &n).await })
                .await;

            match result {
                Ok(Ok(())) => {
                    info!(person_id = %id, name = %name, "person renamed");
                    if let Some(client) = client_weak.upgrade() {
                        client.update_in_models(id.as_str(), |item| {
                            item.set_name(name.clone());
                        });
                    }
                }
                Ok(Err(e)) => {
                    error!("rename_person failed: {e}");
                    on_error(format!("Failed to rename person: {e}"));
                }
                Err(e) => {
                    error!("rename_person join failed: {e}");
                    on_error(format!("Failed to rename person: {e}"));
                }
            }
        });
    }

    /// Hide or unhide a person. On success, repopulates affected models
    /// (because filter state determines visibility).
    pub fn set_person_hidden(
        &self,
        id: PersonId,
        hidden: bool,
        on_error: impl FnOnce(String) + 'static,
    ) {
        let (library, tokio) = self.deps();
        let client_weak: glib::SendWeakRef<PeopleClient> = self.downgrade().into();

        glib::MainContext::default().spawn_local(async move {
            let hide_id = id.clone();
            let result = tokio
                .spawn(async move { library.faces().set_person_hidden(&hide_id, hidden).await })
                .await;

            match result {
                Ok(Ok(())) => {
                    let action = if hidden { "hidden" } else { "unhidden" };
                    info!(person_id = %id, action, "person visibility changed");

                    // Repopulate all tracked models — filter determines membership.
                    if let Some(client) = client_weak.upgrade() {
                        client.repopulate_all_models();
                    }
                }
                Ok(Err(e)) => {
                    error!("set_person_hidden failed: {e}");
                    on_error(format!("Failed to update person visibility: {e}"));
                }
                Err(e) => {
                    error!("set_person_hidden join failed: {e}");
                    on_error(format!("Failed to update person visibility: {e}"));
                }
            }
        });
    }

    // ── Queries ─────────────────────────────────────────────────────────

    /// Resolve a person's thumbnail path (sync, no I/O).
    pub fn person_thumbnail_path(&self, person_id: &PersonId) -> Option<std::path::PathBuf> {
        let deps = self.imp().deps.borrow();
        let deps = deps.as_ref().expect("PeopleClient::configure() not called");
        deps.library.faces().person_thumbnail_path(person_id)
    }

    // ── Model patching (private) ───────────────────────────────────────

    /// Find a person by ID across all tracked models and apply an update.
    fn update_in_models(&self, id: &str, update: impl Fn(&PersonItemObject)) {
        let models = self.imp().models.borrow();
        for tracked in models.iter() {
            if let Some(store) = tracked.store.upgrade() {
                if let Some(item) = find_item_by_id(&store, id) {
                    update(&item);
                }
            }
        }
    }

    /// Repopulate all tracked models using their stored filter state.
    fn repopulate_all_models(&self) {
        // Collect live models + filters, then drop the borrow before calling populate.
        let snapshots: Vec<_> = {
            let models = self.imp().models.borrow();
            models
                .iter()
                .filter_map(|tracked| {
                    let store = tracked.store.upgrade()?;
                    Some((
                        store,
                        tracked.include_hidden.get(),
                        tracked.include_unnamed.get(),
                    ))
                })
                .collect()
        };

        for (store, include_hidden, include_unnamed) in snapshots {
            self.populate(&store, include_hidden, include_unnamed);
        }
    }
}

/// Find a `PersonItemObject` by person ID in a store.
fn find_item_by_id(store: &gio::ListStore, id: &str) -> Option<PersonItemObject> {
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
