use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::transforms::transform_registry;

use super::EditSession;

mod imp {
    use super::*;
    use std::cell::OnceCell;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(
        resource = "/io/github/justinf555/Moments/ui/viewer/edit_panel/transform_section.ui"
    )]
    pub struct EditTransformSection {
        #[template_child]
        pub expander: TemplateChild<adw::ExpanderRow>,
        #[template_child]
        pub grid: TemplateChild<gtk::Grid>,

        pub session: OnceCell<Rc<RefCell<Option<EditSession>>>>,
        pub changed_cb: OnceCell<Rc<dyn Fn()>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EditTransformSection {
        const NAME: &'static str = "MomentsEditTransformSection";
        type Type = super::EditTransformSection;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for EditTransformSection {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for EditTransformSection {}
}

glib::wrapper! {
    pub struct EditTransformSection(ObjectSubclass<imp::EditTransformSection>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl EditTransformSection {
    /// Access the expander row (for single-expansion wiring).
    pub fn expander(&self) -> &adw::ExpanderRow {
        &self.imp().expander
    }

    /// Wire transform buttons and set the state-changed callback.
    ///
    /// The `changed` callback is called after each transform is applied.
    /// The panel uses this to trigger re-rendering and auto-save.
    pub fn setup(&self, session: Rc<RefCell<Option<EditSession>>>, changed: impl Fn() + 'static) {
        let imp = self.imp();
        let changed: Rc<dyn Fn()> = Rc::new(changed);
        let _ = imp.session.set(Rc::clone(&session));
        let _ = imp.changed_cb.set(Rc::clone(&changed));

        let transforms = transform_registry();
        for (i, transform) in transforms.into_iter().enumerate() {
            let row = i as i32 / 2;
            let col = i as i32 % 2;

            let icon = gtk::Image::builder()
                .icon_name(transform.icon_name())
                .pixel_size(24)
                .build();

            let label = gtk::Label::new(Some(transform.label()));
            label.add_css_class("caption");

            let content = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(4)
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .margin_top(8)
                .margin_bottom(8)
                .build();
            content.append(&icon);
            content.append(&label);

            let btn = gtk::Button::builder()
                .child(&content)
                .tooltip_text(transform.label())
                .build();
            btn.add_css_class("flat");

            let session_ref = Rc::clone(&session);
            let changed_ref = Rc::clone(&changed);
            btn.connect_clicked(move |_| {
                {
                    let mut session = session_ref.borrow_mut();
                    let Some(s) = session.as_mut() else { return };
                    transform.apply(&mut s.state.transforms);
                    s.render_gen += 1;
                }
                changed_ref();
            });

            imp.grid.attach(&btn, col, row, 1, 1);
        }
    }
}
