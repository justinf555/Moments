use super::{AdjustGroup, Adjustment, EditState};

pub struct Vibrance;

impl Adjustment for Vibrance {
    fn display_name(&self) -> &'static str {
        "Vibrance"
    }

    fn group(&self) -> AdjustGroup {
        AdjustGroup::Colour
    }

    fn range(&self) -> (f64, f64) {
        (-1.0, 1.0)
    }

    fn get(&self, state: &EditState) -> f64 {
        state.color.vibrance
    }

    fn set(&self, state: &mut EditState, value: f64) {
        state.color.vibrance = value;
    }
}
