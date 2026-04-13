mod brightness;
mod contrast;
mod highlights;
mod saturation;
mod shadows;
mod temperature;
mod tint;
mod vibrance;
mod white_balance;

use crate::library::editing::EditState;

/// Grouping for adjustment sliders in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdjustGroup {
    Light,
    Colour,
}

/// A single adjustment parameter that reads/writes one field of `EditState`.
pub trait Adjustment: Send + Sync {
    /// User-facing label (e.g. `"Brightness"`).
    fn display_name(&self) -> &'static str;

    /// Which UI group this adjustment belongs to.
    fn group(&self) -> AdjustGroup;

    /// Slider range as `(min, max)`.
    fn range(&self) -> (f64, f64);

    /// Read the current value from an `EditState`.
    fn get(&self, state: &EditState) -> f64;

    /// Write a value into an `EditState`.
    fn set(&self, state: &mut EditState, value: f64);
}

/// Return all built-in adjustments in display order (Light group first, then Colour).
pub fn adjustment_registry() -> Vec<Box<dyn Adjustment>> {
    vec![
        // Light group
        Box::new(brightness::Brightness),
        Box::new(contrast::Contrast),
        Box::new(highlights::Highlights),
        Box::new(shadows::Shadows),
        Box::new(white_balance::WhiteBalance),
        // Colour group
        Box::new(saturation::Saturation),
        Box::new(vibrance::Vibrance),
        Box::new(temperature::Temperature),
        Box::new(tint::Tint),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_returns_nine_adjustments() {
        assert_eq!(adjustment_registry().len(), 9);
    }

    #[test]
    fn all_display_names_are_non_empty() {
        for adj in adjustment_registry() {
            assert!(!adj.display_name().is_empty());
        }
    }

    #[test]
    fn light_group_comes_first() {
        let adjustments = adjustment_registry();
        let first_colour = adjustments
            .iter()
            .position(|a| a.group() == AdjustGroup::Colour)
            .unwrap();
        let last_light = adjustments
            .iter()
            .rposition(|a| a.group() == AdjustGroup::Light)
            .unwrap();
        assert!(last_light < first_colour);
    }

    #[test]
    fn get_set_round_trip() {
        for adj in adjustment_registry() {
            let mut state = EditState::default();
            adj.set(&mut state, 0.42);
            let got = adj.get(&state);
            assert!(
                (got - 0.42).abs() < f64::EPSILON,
                "adjustment '{}' round-trip failed: got {got}",
                adj.display_name()
            );
        }
    }

    #[test]
    fn all_ranges_are_valid() {
        for adj in adjustment_registry() {
            let (min, max) = adj.range();
            assert!(
                min < max,
                "adjustment '{}' has invalid range: ({min}, {max})",
                adj.display_name()
            );
        }
    }

    #[test]
    fn default_state_has_zero_values() {
        let state = EditState::default();
        for adj in adjustment_registry() {
            assert_eq!(
                adj.get(&state),
                0.0,
                "adjustment '{}' should default to 0.0",
                adj.display_name()
            );
        }
    }
}
