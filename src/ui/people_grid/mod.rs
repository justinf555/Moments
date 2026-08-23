// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use tracing::warn;

use crate::client::{PeopleClientV2, PersonItemObject};
use crate::ui::photo_grid::texture_cache::TextureCache;

mod actions;
pub mod cell;
pub mod factory;

/// Values of the `people-unnamed-visibility` GSetting.
///
/// Issue #681: a plain boolean can't express "the user hasn't chosen
/// yet". `Auto` is that third state — it resolves once, against the
/// people actually loaded, and stops being consulted the moment the
/// user touches the toggle.
mod unnamed {
    pub const AUTO: u32 = 0;
    pub const ALWAYS: u32 = 1;
    pub const NEVER: u32 = 2;
}

/// Shared filter state for the people grid.
///
/// Toggle buttons mutate this state, then call `changed()` on the
/// `gtk::CustomFilter` so the `FilterListModel` re-evaluates visibility.
pub(crate) struct PeopleFilter {
    include_hidden: Cell<bool>,
    include_unnamed: Cell<bool>,
    /// Set while a toggle is being driven from code rather than by the
    /// user — restoring the persisted state at startup, or resolving
    /// `unnamed::AUTO` once the people have loaded. The `toggled`
    /// handlers skip writing GSettings while it's set, so neither
    /// counts as the user making a choice.
    applying: Cell<bool>,
}

impl PeopleFilter {
    /// Apply `f` to the toggles without it counting as a user choice.
    fn apply_silently(self: &Rc<Self>, f: impl FnOnce()) {
        self.applying.set(true);
        f();
        self.applying.set(false);
    }
}

// ── GObject subclass ─────────────────────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::OnceCell;

    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/people_grid/people_grid.ui")]
    pub struct PeopleGridView {
        #[template_child]
        pub nav_view: TemplateChild<adw::NavigationView>,
        #[template_child]
        pub grid_view: TemplateChild<gtk::GridView>,
        #[template_child]
        pub unnamed_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub hidden_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub empty_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub empty_show_all_btn: TemplateChild<gtk::Button>,

        // Service dependencies
        pub people_client: OnceCell<PeopleClientV2>,

        // State
        pub(super) store: OnceCell<gio::ListStore>,
        pub(super) filter_model: OnceCell<gtk::FilterListModel>,
        pub(super) filter_state: OnceCell<Rc<PeopleFilter>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PeopleGridView {
        const NAME: &'static str = "MomentsPeopleGridView";
        type Type = super::PeopleGridView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PeopleGridView {
        fn dispose(&self) {
            self.dispose_template();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for PeopleGridView {}
}

glib::wrapper! {
    pub struct PeopleGridView(ObjectSubclass<imp::PeopleGridView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PeopleGridView {
    fn default() -> Self {
        Self::new()
    }
}

impl PeopleGridView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Set up the People collection grid view.
    pub fn setup_people(&self, settings: gio::Settings, texture_cache: Rc<TextureCache>) {
        let imp = self.imp();

        let people_client = crate::application::MomentsApplication::default()
            .people_client()
            .expect("people client available after library load");
        assert!(
            imp.people_client.set(people_client.clone()).is_ok(),
            "setup called twice"
        );

        let unnamed_setting = settings.uint("people-unnamed-visibility");
        let filter_state = Rc::new(PeopleFilter {
            include_hidden: Cell::new(settings.boolean("people-show-hidden")),
            include_unnamed: Cell::new(unnamed_visible_initially(unnamed_setting)),
            applying: Cell::new(false),
        });

        // Client returns all people — filtering happens here via FilterListModel.
        let store = people_client.create_model();

        let fs = Rc::clone(&filter_state);
        let custom_filter = gtk::CustomFilter::new(move |obj| {
            obj.downcast_ref::<PersonItemObject>()
                .is_some_and(|person| {
                    person_passes(person, fs.include_hidden.get(), fs.include_unnamed.get())
                })
        });

        let filter_model =
            gtk::FilterListModel::new(Some(store.clone()), Some(custom_filter.clone()));

        let cell_size = 140;
        let factory = factory::build_factory(cell_size);
        let selection = gtk::NoSelection::new(Some(filter_model.clone()));
        imp.grid_view.set_model(Some(&selection));
        imp.grid_view.set_factory(Some(&factory));

        self.wire_toggles(&filter_state, &custom_filter, &settings);
        // Resolution runs before the empty-state refresh so the status
        // page never flashes for a filter that's about to be lifted.
        if unnamed_setting == unnamed::AUTO {
            self.wire_auto_unnamed(&store, &filter_state);
        }
        self.wire_empty_state(&store, &filter_model);

        // Wire item activation and context menu.
        actions::wire_activation(
            &imp.grid_view,
            &filter_model,
            &imp.nav_view,
            &settings,
            &texture_cache,
        );
        actions::wire_context_menu(&imp.grid_view, &filter_model, &people_client);

        // Initial populate.
        people_client.list_people(&store);

        assert!(imp.store.set(store).is_ok());
        assert!(imp.filter_model.set(filter_model).is_ok());
        assert!(imp.filter_state.set(filter_state).is_ok());
    }

    /// Wire both header toggles to the filter state, restore their
    /// persisted positions, and persist any later change.
    fn wire_toggles(
        &self,
        filter_state: &Rc<PeopleFilter>,
        custom_filter: &gtk::CustomFilter,
        settings: &gio::Settings,
    ) {
        let imp = self.imp();

        {
            let fs = Rc::clone(filter_state);
            let cf = custom_filter.clone();
            let s = settings.clone();
            imp.unnamed_toggle.connect_toggled(move |btn| {
                fs.include_unnamed.set(btn.is_active());
                cf.changed(filter_change(btn.is_active()));
                if !fs.applying.get() {
                    let value = if btn.is_active() {
                        unnamed::ALWAYS
                    } else {
                        unnamed::NEVER
                    };
                    if let Err(e) = s.set_uint("people-unnamed-visibility", value) {
                        warn!("failed to save people-unnamed-visibility: {e}");
                    }
                }
            });
        }
        {
            let fs = Rc::clone(filter_state);
            let cf = custom_filter.clone();
            let s = settings.clone();
            imp.hidden_toggle.connect_toggled(move |btn| {
                fs.include_hidden.set(btn.is_active());
                cf.changed(filter_change(btn.is_active()));
                if !fs.applying.get() {
                    if let Err(e) = s.set_boolean("people-show-hidden", btn.is_active()) {
                        warn!("failed to save people-show-hidden: {e}");
                    }
                }
            });
        }

        // Restoring the persisted state isn't the user re-choosing it.
        filter_state.apply_silently(|| {
            imp.unnamed_toggle
                .set_active(filter_state.include_unnamed.get());
            imp.hidden_toggle
                .set_active(filter_state.include_hidden.get());
        });

        // The empty page's escape hatch: turn every filter off.
        let unnamed_toggle = imp.unnamed_toggle.get();
        let hidden_toggle = imp.hidden_toggle.get();
        imp.empty_show_all_btn.connect_clicked(glib::clone!(
            #[weak]
            unnamed_toggle,
            #[weak]
            hidden_toggle,
            move |_| {
                unnamed_toggle.set_active(true);
                hidden_toggle.set_active(true);
            }
        ));
    }

    /// Resolve `unnamed::AUTO` once the people have loaded: show unnamed
    /// people while nobody has been named, hide them once somebody has.
    ///
    /// Issue #681: a fresh Immich sync is all unnamed people, so the
    /// pre-#681 default hid 100% of the library and the view looked
    /// broken. The filter still earns its keep on a named library, where
    /// it stops a long tail of anonymous faces burying the people you
    /// care about — hence the split rather than a flat default-on.
    ///
    /// Resolved once per view, not re-evaluated live: naming your first
    /// person shouldn't make everyone else vanish mid-session.
    fn wire_auto_unnamed(&self, store: &gio::ListStore, filter_state: &Rc<PeopleFilter>) {
        let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));

        let toggle = self.imp().unnamed_toggle.get();
        let id = store.connect_items_changed(glib::clone!(
            #[weak]
            toggle,
            #[strong]
            handler,
            #[strong]
            filter_state,
            move |store, _, _, _| {
                if store.n_items() == 0 {
                    return; // Nothing to resolve against yet.
                }
                if let Some(id) = handler.borrow_mut().take() {
                    store.disconnect(id);
                }
                // Silently: resolving the automatic default is not the
                // user choosing, so the setting stays on `AUTO` and gets
                // re-resolved next launch.
                let show = !any_named(store);
                filter_state.apply_silently(|| toggle.set_active(show));
            }
        ));
        *handler.borrow_mut() = Some(id);
    }

    /// Swap in a status page whenever the grid would render bare, and
    /// tell the two cases apart: a library with no people at all, versus
    /// one whose people are all filtered out.
    fn wire_empty_state(&self, store: &gio::ListStore, filter_model: &gtk::FilterListModel) {
        let imp = self.imp();
        let refresh = {
            let stack = imp.content_stack.get();
            let page = imp.empty_page.get();
            let button = imp.empty_show_all_btn.get();
            // Weak: both models end up owning this closure through their
            // own `items-changed` handlers, and a strong capture would
            // make each a cycle with itself.
            let store = store.downgrade();
            let filter_model = filter_model.downgrade();
            move || {
                let (Some(store), Some(filter_model)) = (store.upgrade(), filter_model.upgrade())
                else {
                    return;
                };
                if filter_model.n_items() > 0 {
                    stack.set_visible_child_name("grid");
                    return;
                }
                let filtered_out = store.n_items() > 0;
                if filtered_out {
                    page.set_title(&gettext("Everyone Is Filtered Out"));
                    page.set_description(Some(&gettext(
                        "Unnamed or hidden people are being left out of this view.",
                    )));
                } else {
                    page.set_title(&gettext("No People Yet"));
                    page.set_description(Some(&gettext(
                        "People appear here once faces have been detected in your photos.",
                    )));
                }
                button.set_visible(filtered_out);
                stack.set_visible_child_name("empty");
            }
        };

        // The filter model alone isn't enough: people arriving that the
        // filter rejects leave its count at zero, so the status page
        // would keep claiming there are none at all.
        let r = refresh.clone();
        store.connect_items_changed(move |_, _, _, _| r());
        let r = refresh.clone();
        filter_model.connect_items_changed(move |_, _, _, _| r());
        refresh();
    }
}

/// Whether a person survives the two header filters.
fn person_passes(person: &PersonItemObject, include_hidden: bool, include_unnamed: bool) -> bool {
    if person.is_hidden() && !include_hidden {
        return false;
    }
    if person.name().is_empty() && !include_unnamed {
        return false;
    }
    true
}

/// Where the unnamed toggle starts for a persisted setting value.
///
/// `AUTO` starts permissive and is resolved against the real people once
/// they load — while we don't know yet, an empty grid is the failure
/// mode worth avoiding. Anything unrecognised is treated the same way.
fn unnamed_visible_initially(setting: u32) -> bool {
    setting != unnamed::NEVER
}

/// Whether at least one loaded person has been given a name.
fn any_named(store: &gio::ListStore) -> bool {
    (0..store.n_items()).any(|i| {
        store
            .item(i)
            .and_then(|o| o.downcast::<PersonItemObject>().ok())
            .is_some_and(|p| !p.name().is_empty())
    })
}

/// How a toggle's new state changes the filter's strictness.
///
/// Both toggles are "show me more" switches — Show Unnamed, Show Hidden
/// — so activating one *relaxes* the filter and admits items it was
/// rejecting.
fn filter_change(now_active: bool) -> gtk::FilterChange {
    if now_active {
        gtk::FilterChange::LessStrict
    } else {
        gtk::FilterChange::MoreStrict
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::faces::{Person, PersonId};

    fn person(name: &str, is_hidden: bool) -> PersonItemObject {
        PersonItemObject::new(
            &Person {
                id: PersonId::from_raw("p1".to_string()),
                name: name.to_string(),
                face_count: 0,
                is_hidden,
            },
            None,
        )
    }

    fn store_of(names: &[&str]) -> gio::ListStore {
        let store = gio::ListStore::new::<PersonItemObject>();
        for name in names {
            store.append(&person(name, false));
        }
        store
    }

    // ── person_passes ─────────────────────────────────────────────────

    #[test]
    fn named_visible_person_always_passes() {
        assert!(person_passes(&person("Alice", false), false, false));
    }

    #[test]
    fn unnamed_person_passes_only_when_included() {
        assert!(!person_passes(&person("", false), false, false));
        assert!(person_passes(&person("", false), false, true));
    }

    #[test]
    fn hidden_person_passes_only_when_included() {
        assert!(!person_passes(&person("Alice", true), false, false));
        assert!(person_passes(&person("Alice", true), true, false));
    }

    /// Both filters apply — clearing one isn't enough for a person that
    /// trips the other.
    #[test]
    fn hidden_and_unnamed_needs_both_filters_off() {
        assert!(!person_passes(&person("", true), true, false));
        assert!(!person_passes(&person("", true), false, true));
        assert!(person_passes(&person("", true), true, true));
    }

    // ── unnamed_visible_initially ─────────────────────────────────────

    #[test]
    fn only_an_explicit_never_starts_unnamed_hidden() {
        assert!(unnamed_visible_initially(unnamed::AUTO));
        assert!(unnamed_visible_initially(unnamed::ALWAYS));
        assert!(!unnamed_visible_initially(unnamed::NEVER));
        assert!(
            unnamed_visible_initially(99),
            "unknown values stay permissive"
        );
    }

    // ── any_named ─────────────────────────────────────────────────────

    /// Issue #681: a freshly synced Immich library is all unnamed
    /// people. `AUTO` must resolve to "show them" or the grid renders
    /// empty and looks broken.
    #[test]
    fn fresh_library_of_unnamed_people_resolves_to_showing_them() {
        assert!(!any_named(&store_of(&["", "", ""])));
    }

    #[test]
    fn one_named_person_is_enough_to_hide_the_rest() {
        assert!(any_named(&store_of(&["", "Alice", ""])));
    }

    #[test]
    fn empty_store_has_nobody_named() {
        assert!(!any_named(&store_of(&[])));
    }

    // ── filter_change ─────────────────────────────────────────────────

    #[test]
    fn turning_a_toggle_on_admits_more_items() {
        assert_eq!(filter_change(true), gtk::FilterChange::LessStrict);
        assert_eq!(filter_change(false), gtk::FilterChange::MoreStrict);
    }
}
