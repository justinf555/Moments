use super::{EditState, Filter};

pub struct Cool;

impl Filter for Cool {
    fn name(&self) -> &'static str {
        "cool"
    }

    fn display_name(&self) -> &'static str {
        "Cool"
    }

    fn preset(&self) -> EditState {
        let mut state = EditState {
            filter: Some("cool".to_string()),
            ..Default::default()
        };
        state.color.temperature = -0.4;
        state.color.saturation = 0.1;
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cool_shifts_temperature_negative() {
        let preset = Cool.preset();
        assert!(preset.color.temperature < 0.0);
    }
}
