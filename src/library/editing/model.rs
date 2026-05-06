use serde::{Deserialize, Serialize};

/// Normalized crop rectangle with coordinates in the 0.0–1.0 range,
/// relative to the image dimensions after orientation correction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CropRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Geometric transforms: crop, rotate, straighten, flip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TransformState {
    /// Crop rectangle in normalized coordinates, or `None` for no crop.
    pub crop: Option<CropRect>,
    /// Rotation in 90-degree steps: 0, 90, 180, or 270.
    pub rotate_degrees: i32,
    /// Freeform straighten angle in degrees (-45.0 to 45.0).
    pub straighten_degrees: f64,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
}

/// Exposure adjustments. All values range from -1.0 to 1.0 with 0.0 as neutral.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ExposureState {
    pub brightness: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
}

/// Color adjustments. All values range from -1.0 to 1.0 with 0.0 as neutral.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ColorState {
    pub saturation: f64,
    pub vibrance: f64,
    pub hue_shift: f64,
    pub temperature: f64,
    pub tint: f64,
}

/// Detail-section adjustments — vignette, clarity, sharpness, noise reduction.
///
/// Fields land per-feature (#252 vignette, #474 clarity, #250 sharpness,
/// #251 noise reduction). Each field uses `#[serde(default)]` so adding
/// the next one doesn't require a schema migration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DetailState {
    /// Radial darkening (positive) or brightening (negative) of the
    /// corners. -1.0 to 1.0, 0.0 = no effect.
    #[serde(default)]
    pub vignette: f64,
}

/// Complete non-destructive edit state for a media asset.
///
/// Stored as JSON in the `edits` table. All fields default to identity
/// values (no visible change). Filters are preset combinations of
/// exposure/color values — selecting a filter sets those sections, but
/// the user can further tweak individual sliders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditState {
    /// Schema version for forward compatibility.
    pub version: u32,
    #[serde(default)]
    pub transforms: TransformState,
    #[serde(default)]
    pub exposure: ExposureState,
    #[serde(default)]
    pub color: ColorState,
    #[serde(default)]
    pub detail: DetailState,
    /// Name of the applied filter preset, or `None`.
    #[serde(default)]
    pub filter: Option<String>,
    /// Filter intensity (0.0–1.0). Scales the preset's exposure/color
    /// values. Defaults to 1.0 (full strength).
    #[serde(default = "default_filter_strength")]
    pub filter_strength: f64,
}

fn default_filter_strength() -> f64 {
    1.0
}

impl Default for EditState {
    fn default() -> Self {
        Self {
            version: 1,
            transforms: TransformState::default(),
            exposure: ExposureState::default(),
            color: ColorState::default(),
            detail: DetailState::default(),
            filter: None,
            filter_strength: 1.0,
        }
    }
}

impl EditState {
    /// Returns `true` if this edit state represents no visible change.
    pub fn is_identity(&self) -> bool {
        self.transforms == TransformState::default()
            && self.exposure == ExposureState::default()
            && self.color == ColorState::default()
            && self.detail == DetailState::default()
            && self.filter.is_none()
    }

    /// Apply a filter preset's exposure/color/detail values scaled by strength.
    ///
    /// Sets this state's exposure, color, and detail fields to the preset
    /// values multiplied by `strength` (0.0–1.0), and records the filter
    /// strength.
    ///
    /// **Filter-preset policy for new sections:** when fields land in
    /// `DetailState` (#474 clarity, #250 sharpness, #251 noise reduction)
    /// and a future `HslState` (#473), decide per-field whether filters
    /// should scale into them. Defaults so far: scale vignette (stylistic
    /// — Vintage/Noir presets often include darkened corners); scale
    /// clarity when it lands (also stylistic); don't scale noise reduction
    /// (per-photo cleanup, not stylistic). Update this method when each
    /// field lands.
    pub fn apply_filter_at_strength(&mut self, preset: &EditState, strength: f64) {
        self.exposure.brightness = preset.exposure.brightness * strength;
        self.exposure.contrast = preset.exposure.contrast * strength;
        self.exposure.highlights = preset.exposure.highlights * strength;
        self.exposure.shadows = preset.exposure.shadows * strength;
        self.color.saturation = preset.color.saturation * strength;
        self.color.vibrance = preset.color.vibrance * strength;
        self.color.hue_shift = preset.color.hue_shift * strength;
        self.color.temperature = preset.color.temperature * strength;
        self.color.tint = preset.color.tint * strength;
        self.detail.vignette = preset.detail.vignette * strength;
        self.filter_strength = strength;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_edit_state_is_identity() {
        let state = EditState::default();
        assert!(state.is_identity());
        assert_eq!(state.version, 1);
    }

    #[test]
    fn modified_state_is_not_identity() {
        let mut state = EditState::default();
        state.exposure.brightness = 0.5;
        assert!(!state.is_identity());
    }

    #[test]
    fn filter_only_is_not_identity() {
        let state = EditState {
            filter: Some("bw".to_string()),
            ..Default::default()
        };
        assert!(!state.is_identity());
    }

    #[test]
    fn serialize_round_trip() {
        let state = EditState {
            version: 1,
            transforms: TransformState {
                crop: Some(CropRect {
                    x: 0.1,
                    y: 0.2,
                    width: 0.8,
                    height: 0.6,
                }),
                rotate_degrees: 90,
                straighten_degrees: 2.5,
                flip_horizontal: true,
                flip_vertical: false,
            },
            exposure: ExposureState {
                brightness: 0.3,
                contrast: -0.2,
                highlights: 0.1,
                shadows: -0.1,
            },
            color: ColorState {
                saturation: 0.5,
                vibrance: 0.2,
                hue_shift: -0.1,
                temperature: 0.3,
                tint: -0.05,
            },
            detail: DetailState { vignette: 0.4 },
            filter: Some("vintage".to_string()),
            filter_strength: 0.75,
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: EditState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, restored);
    }

    #[test]
    fn filter_scales_into_vignette() {
        let preset = EditState {
            detail: DetailState { vignette: 0.6 },
            ..Default::default()
        };
        let mut state = EditState::default();
        state.apply_filter_at_strength(&preset, 0.5);
        assert!((state.detail.vignette - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn deserialize_with_missing_fields_uses_defaults() {
        let json = r#"{"version": 1}"#;
        let state: EditState = serde_json::from_str(json).unwrap();
        assert!(state.is_identity());
    }

    #[test]
    fn deserialize_v1_json_without_detail_uses_default_detail() {
        // Simulates an EditState row written before DetailState existed:
        // a real edit (filter set), no `detail` key. The new field must
        // default rather than fail deserialization.
        let json = r#"{
            "version": 1,
            "filter": "vintage"
        }"#;
        let state: EditState = serde_json::from_str(json).unwrap();
        assert_eq!(state.detail, DetailState::default());
        assert_eq!(state.filter.as_deref(), Some("vintage"));
        assert!(!state.is_identity());
    }
}
