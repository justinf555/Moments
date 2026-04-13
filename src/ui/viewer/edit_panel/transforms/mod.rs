mod flip_horizontal;
mod flip_vertical;
mod rotate_ccw;
mod rotate_cw;

use crate::library::editing::TransformState;

/// A geometric transform operation (rotate, flip) that mutates a `TransformState`.
pub trait Transform: Send + Sync {
    /// Adwaita icon name for the button.
    fn icon_name(&self) -> &'static str;

    /// User-facing button label.
    fn label(&self) -> &'static str;

    /// Apply this transform to the given state.
    fn apply(&self, state: &mut TransformState);
}

/// Return all built-in transforms in display order (2x2 grid layout).
pub fn transform_registry() -> Vec<Box<dyn Transform>> {
    vec![
        Box::new(rotate_ccw::RotateCcw),
        Box::new(rotate_cw::RotateCw),
        Box::new(flip_horizontal::FlipHorizontal),
        Box::new(flip_vertical::FlipVertical),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_returns_four_transforms() {
        assert_eq!(transform_registry().len(), 4);
    }

    #[test]
    fn all_labels_are_non_empty() {
        for t in transform_registry() {
            assert!(!t.icon_name().is_empty());
            assert!(!t.label().is_empty());
        }
    }

    #[test]
    fn each_transform_modifies_default_state() {
        for t in transform_registry() {
            let mut state = TransformState::default();
            t.apply(&mut state);
            assert_ne!(
                state,
                TransformState::default(),
                "transform '{}' should modify default state",
                t.label()
            );
        }
    }
}
