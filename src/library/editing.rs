use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::error::LibraryError;
use super::media::MediaId;

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
    pub white_balance: f64,
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
            && self.filter.is_none()
    }
}

/// Feature trait for non-destructive photo editing.
///
/// Edit operations are stored as JSON and applied on the fly during
/// display. For the Immich backend, `render_and_save` uploads the
/// rendered result as an edited version. For the local backend, edits
/// are applied during viewing and thumbnail generation.
#[async_trait]
pub trait LibraryEditing: Send + Sync {
    /// Get the current edit state for a media item.
    /// Returns `None` if no edits have been applied.
    async fn get_edit_state(&self, id: &MediaId) -> Result<Option<EditState>, LibraryError>;

    /// Save the current edit state for a media item.
    /// Overwrites any existing state.
    async fn save_edit_state(&self, id: &MediaId, state: &EditState) -> Result<(), LibraryError>;

    /// Remove all edits for a media item (revert to original).
    async fn revert_edits(&self, id: &MediaId) -> Result<(), LibraryError>;

    /// Render the current edit state to a full-resolution image and persist it.
    /// For Immich: uploads as edited version. For local: no-op (edits applied on the fly).
    async fn render_and_save(&self, id: &MediaId) -> Result<(), LibraryError>;

    /// Check whether an asset has unsaved/unrendered edits.
    async fn has_pending_edits(&self, id: &MediaId) -> Result<bool, LibraryError>;
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
                white_balance: 0.0,
            },
            color: ColorState {
                saturation: 0.5,
                vibrance: 0.2,
                hue_shift: -0.1,
                temperature: 0.3,
                tint: -0.05,
            },
            filter: Some("vintage".to_string()),
            filter_strength: 0.75,
        };

        let json = serde_json::to_string(&state).unwrap();
        let restored: EditState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, restored);
    }

    #[test]
    fn deserialize_with_missing_fields_uses_defaults() {
        let json = r#"{"version": 1}"#;
        let state: EditState = serde_json::from_str(json).unwrap();
        assert!(state.is_identity());
    }
}
