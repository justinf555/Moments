// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use super::{Transform, TransformState};

pub struct FlipHorizontal;

impl Transform for FlipHorizontal {
    fn icon_name(&self) -> &'static str {
        "object-flip-horizontal-symbolic"
    }

    fn label(&self) -> &'static str {
        "Flip H"
    }

    fn apply(&self, state: &mut TransformState) {
        state.flip_horizontal = !state.flip_horizontal;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flip_toggles() {
        let mut state = TransformState::default();
        assert!(!state.flip_horizontal);
        FlipHorizontal.apply(&mut state);
        assert!(state.flip_horizontal);
        FlipHorizontal.apply(&mut state);
        assert!(!state.flip_horizontal);
    }
}
