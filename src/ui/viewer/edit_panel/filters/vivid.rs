use super::{EditState, Filter};

pub struct Vivid;

impl Filter for Vivid {
    fn name(&self) -> &'static str {
        "vivid"
    }

    fn display_name(&self) -> &'static str {
        "Vivid"
    }

    fn preset(&self) -> EditState {
        let mut state = EditState {
            filter: Some("vivid".to_string()),
            ..Default::default()
        };
        state.color.saturation = 0.5;
        state.color.vibrance = 0.3;
        state.exposure.contrast = 0.15;
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vivid_boosts_saturation() {
        let preset = Vivid.preset();
        assert!(preset.color.saturation > 0.0);
        assert!(preset.color.vibrance > 0.0);
    }
}
