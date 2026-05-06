use super::{AdjustGroup, Adjustment, EditState};

pub struct Vignette;

impl Adjustment for Vignette {
    fn display_name(&self) -> &'static str {
        "Vignette"
    }

    fn group(&self) -> AdjustGroup {
        AdjustGroup::Detail
    }

    fn range(&self) -> (f64, f64) {
        (-1.0, 1.0)
    }

    fn get(&self, state: &EditState) -> f64 {
        state.detail.vignette
    }

    fn set(&self, state: &mut EditState, value: f64) {
        state.detail.vignette = value;
    }
}
