use super::{EditState, Filter};

pub struct Matte;

impl Filter for Matte {
    fn name(&self) -> &'static str {
        "matte"
    }

    fn display_name(&self) -> &'static str {
        "Matte"
    }

    fn preset(&self) -> EditState {
        let mut state = EditState {
            filter: Some("matte".to_string()),
            ..Default::default()
        };
        state.exposure.contrast = -0.15;
        state.exposure.shadows = 0.3;
        state.color.saturation = -0.1;
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matte_lifts_shadows() {
        let preset = Matte.preset();
        assert!(preset.exposure.shadows > 0.0);
    }
}
