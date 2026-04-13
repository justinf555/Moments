use super::{EditState, Filter};

pub struct Warm;

impl Filter for Warm {
    fn name(&self) -> &'static str {
        "warm"
    }

    fn display_name(&self) -> &'static str {
        "Warm"
    }

    fn preset(&self) -> EditState {
        let mut state = EditState {
            filter: Some("warm".to_string()),
            ..Default::default()
        };
        state.color.temperature = 0.4;
        state.color.saturation = 0.1;
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_shifts_temperature_positive() {
        let preset = Warm.preset();
        assert!(preset.color.temperature > 0.0);
    }
}
