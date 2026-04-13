use super::{EditState, Filter};

pub struct Noir;

impl Filter for Noir {
    fn name(&self) -> &'static str {
        "noir"
    }

    fn display_name(&self) -> &'static str {
        "Noir"
    }

    fn preset(&self) -> EditState {
        let mut state = EditState {
            filter: Some("noir".to_string()),
            ..Default::default()
        };
        state.color.saturation = -1.0;
        state.exposure.contrast = 0.3;
        state.exposure.brightness = -0.05;
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noir_is_high_contrast_monochrome() {
        let preset = Noir.preset();
        assert_eq!(preset.color.saturation, -1.0);
        assert!(preset.exposure.contrast > 0.2);
    }
}
