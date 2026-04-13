use super::{EditState, Filter};

pub struct Chrome;

impl Filter for Chrome {
    fn name(&self) -> &'static str {
        "chrome"
    }

    fn display_name(&self) -> &'static str {
        "Chrome"
    }

    fn preset(&self) -> EditState {
        let mut state = EditState {
            filter: Some("chrome".to_string()),
            ..Default::default()
        };
        state.exposure.contrast = 0.25;
        state.color.saturation = -0.15;
        state.exposure.highlights = 0.2;
        state.exposure.shadows = -0.2;
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_has_high_contrast_with_desaturation() {
        let preset = Chrome.preset();
        assert!(preset.exposure.contrast > 0.0);
        assert!(preset.color.saturation < 0.0);
    }
}
