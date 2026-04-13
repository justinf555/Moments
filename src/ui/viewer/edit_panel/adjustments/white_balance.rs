use super::{AdjustGroup, Adjustment, EditState};

pub struct WhiteBalance;

impl Adjustment for WhiteBalance {
    fn display_name(&self) -> &'static str {
        "White Balance"
    }

    fn group(&self) -> AdjustGroup {
        AdjustGroup::Light
    }

    fn range(&self) -> (f64, f64) {
        (-1.0, 1.0)
    }

    fn get(&self, state: &EditState) -> f64 {
        state.exposure.white_balance
    }

    fn set(&self, state: &mut EditState, value: f64) {
        state.exposure.white_balance = value;
    }
}
