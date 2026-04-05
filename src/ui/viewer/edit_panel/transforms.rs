use std::sync::Arc;

use adw::prelude::*;
use adw::subclass::prelude::*;

use super::{EditPanel, EditSession};

impl EditPanel {
    /// Wire rotate/flip buttons from the Blueprint template.
    pub(super) fn wire_transform_buttons(&self) {
        let imp = self.imp();

        wire_transform_button(&imp.rotate_ccw_btn, self, |s| {
            s.state.transforms.rotate_degrees =
                (s.state.transforms.rotate_degrees - 90).rem_euclid(360);
        });

        wire_transform_button(&imp.rotate_cw_btn, self, |s| {
            s.state.transforms.rotate_degrees =
                (s.state.transforms.rotate_degrees + 90).rem_euclid(360);
        });

        wire_transform_button(&imp.flip_h_btn, self, |s| {
            s.state.transforms.flip_horizontal = !s.state.transforms.flip_horizontal;
        });

        wire_transform_button(&imp.flip_v_btn, self, |s| {
            s.state.transforms.flip_vertical = !s.state.transforms.flip_vertical;
        });
    }
}

/// Wire a transform button to mutate the edit state and re-render.
fn wire_transform_button<F>(btn: &gtk::Button, panel: &EditPanel, mutate: F)
where
    F: Fn(&mut EditSession) + 'static,
{
    let weak = panel.downgrade();
    let auto_save = panel.auto_save_closure();

    btn.connect_clicked(move |_| {
        let Some(panel) = weak.upgrade() else { return };
        let imp = panel.imp();
        let preview = {
            let mut session = imp.session.borrow_mut();
            let Some(s) = session.as_mut() else { return };
            mutate(s);
            s.render_gen += 1;
            (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
        };
        panel.render_to_picture(preview);
        auto_save();
    });
}
