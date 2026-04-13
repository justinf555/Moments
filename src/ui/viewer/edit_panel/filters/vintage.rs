use super::{EditState, Filter};

pub struct Vintage;

impl Filter for Vintage {
    fn name(&self) -> &'static str {
        "vintage"
    }

    fn display_name(&self) -> &'static str {
        "Vintage"
    }

    fn preset(&self) -> EditState {
        let mut state = EditState {
            filter: Some("vintage".to_string()),
            ..Default::default()
        };
        state.color.saturation = -0.3;
        state.color.temperature = 0.3;
        state.exposure.contrast = -0.1;
        state.exposure.brightness = 0.05;
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vintage_is_warm_and_desaturated() {
        let preset = Vintage.preset();
        assert!(preset.color.saturation < 0.0);
        assert!(preset.color.temperature > 0.0);
    }
}
