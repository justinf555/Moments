use super::{Transform, TransformState};

pub struct RotateCcw;

impl Transform for RotateCcw {
    fn icon_name(&self) -> &'static str {
        "object-rotate-left-symbolic"
    }

    fn label(&self) -> &'static str {
        "Rotate CCW"
    }

    fn apply(&self, state: &mut TransformState) {
        state.rotate_degrees = (state.rotate_degrees - 90).rem_euclid(360);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_ccw_from_zero() {
        let mut state = TransformState::default();
        RotateCcw.apply(&mut state);
        assert_eq!(state.rotate_degrees, 270);
    }

    #[test]
    fn rotate_ccw_wraps_around() {
        let mut state = TransformState::default();
        for _ in 0..4 {
            RotateCcw.apply(&mut state);
        }
        assert_eq!(state.rotate_degrees, 0);
    }
}
