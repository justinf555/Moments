use super::{EditState, Filter};

pub struct Golden;

impl Filter for Golden {
    fn name(&self) -> &'static str {
        "golden"
    }

    fn display_name(&self) -> &'static str {
        "Golden"
    }

    fn preset(&self) -> EditState {
        let mut state = EditState {
            filter: Some("golden".to_string()),
            ..Default::default()
        };
        state.color.temperature = 0.5;
        state.color.saturation = 0.15;
        state.exposure.brightness = 0.05;
        state.exposure.contrast = 0.1;
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_is_warm_and_bright() {
        let preset = Golden.preset();
        assert!(preset.color.temperature > 0.3);
        assert!(preset.exposure.brightness > 0.0);
    }
}
