// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

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
