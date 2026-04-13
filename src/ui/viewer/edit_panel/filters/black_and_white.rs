use super::{EditState, Filter};

pub struct BlackAndWhite;

impl Filter for BlackAndWhite {
    fn name(&self) -> &'static str {
        "bw"
    }

    fn display_name(&self) -> &'static str {
        "B&W"
    }

    fn preset(&self) -> EditState {
        let mut state = EditState {
            filter: Some("bw".to_string()),
            ..Default::default()
        };
        state.color.saturation = -1.0;
        state.exposure.contrast = 0.1;
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bw_desaturates_fully() {
        let preset = BlackAndWhite.preset();
        assert_eq!(preset.color.saturation, -1.0);
    }
}
