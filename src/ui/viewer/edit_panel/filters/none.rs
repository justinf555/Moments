use super::{EditState, Filter};

pub struct None;

impl Filter for None {
    fn name(&self) -> &'static str {
        "none"
    }

    fn display_name(&self) -> &'static str {
        "None"
    }

    fn preset(&self) -> EditState {
        EditState::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_identity() {
        let preset = None.preset();
        assert!(preset.is_identity());
    }
}
