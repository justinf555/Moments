use super::{Transform, TransformState};

pub struct RotateCw;

impl Transform for RotateCw {
    fn icon_name(&self) -> &'static str {
        "object-rotate-right-symbolic"
    }

    fn label(&self) -> &'static str {
        "Rotate CW"
    }

    fn apply(&self, state: &mut TransformState) {
        state.rotate_degrees = (state.rotate_degrees + 90).rem_euclid(360);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_cw_from_zero() {
        let mut state = TransformState::default();
        RotateCw.apply(&mut state);
        assert_eq!(state.rotate_degrees, 90);
    }

    #[test]
    fn rotate_cw_wraps_around() {
        let mut state = TransformState::default();
        for _ in 0..4 {
            RotateCw.apply(&mut state);
        }
        assert_eq!(state.rotate_degrees, 0);
    }
}
