use super::{AdjustGroup, Adjustment, EditState};

pub struct Shadows;

impl Adjustment for Shadows {
    fn display_name(&self) -> &'static str {
        "Shadows"
    }

    fn group(&self) -> AdjustGroup {
        AdjustGroup::Light
    }

    fn range(&self) -> (f64, f64) {
        (-1.0, 1.0)
    }

    fn get(&self, state: &EditState) -> f64 {
        state.exposure.shadows
    }

    fn set(&self, state: &mut EditState, value: f64) {
        state.exposure.shadows = value;
    }
}
