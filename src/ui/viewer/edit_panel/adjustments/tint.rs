use super::{AdjustGroup, Adjustment, EditState};

pub struct Tint;

impl Adjustment for Tint {
    fn display_name(&self) -> &'static str {
        "Tint"
    }

    fn group(&self) -> AdjustGroup {
        AdjustGroup::Colour
    }

    fn range(&self) -> (f64, f64) {
        (-1.0, 1.0)
    }

    fn get(&self, state: &EditState) -> f64 {
        state.color.tint
    }

    fn set(&self, state: &mut EditState, value: f64) {
        state.color.tint = value;
    }
}
