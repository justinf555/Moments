// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use super::{Transform, TransformState};

pub struct FlipVertical;

impl Transform for FlipVertical {
    fn icon_name(&self) -> &'static str {
        "object-flip-vertical-symbolic"
    }

    fn label(&self) -> &'static str {
        "Flip V"
    }

    fn apply(&self, state: &mut TransformState) {
        state.flip_vertical = !state.flip_vertical;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flip_toggles() {
        let mut state = TransformState::default();
        assert!(!state.flip_vertical);
        FlipVertical.apply(&mut state);
        assert!(state.flip_vertical);
        FlipVertical.apply(&mut state);
        assert!(!state.flip_vertical);
    }
}
