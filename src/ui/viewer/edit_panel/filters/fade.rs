use super::{EditState, Filter};

pub struct Fade;

impl Filter for Fade {
    fn name(&self) -> &'static str {
        "fade"
    }

    fn display_name(&self) -> &'static str {
        "Fade"
    }

    fn preset(&self) -> EditState {
        let mut state = EditState {
            filter: Some("fade".to_string()),
            ..Default::default()
        };
        state.exposure.contrast = -0.2;
        state.exposure.brightness = 0.1;
        state.color.saturation = -0.2;
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_reduces_contrast() {
        let preset = Fade.preset();
        assert!(preset.exposure.contrast < 0.0);
    }
}
