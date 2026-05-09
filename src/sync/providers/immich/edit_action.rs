//! Immich-shaped projection of [`EditState`].
//!
//! Immich's `PUT /assets/{id}/edits` API takes an ordered array of
//! `{action, parameters}` records. This module owns the wire shape and
//! the projection rules — they're sync-layer concerns and never leak
//! into the library.
//!
//! The order in the emitted array must match the local renderer's
//! application order so that round-trip pull/push is consistent. The
//! local renderer applies **rotate → flip → crop** on top of the
//! EXIF-oriented image (see `src/renderer/edits.rs::apply_transforms`),
//! so we emit actions in that order.
//!
//! Returns `None` for any state that isn't expressible as Immich's
//! geometric action list (pixel adjustments, filters, freeform
//! straighten). The push handler treats `None` as "Phase C territory —
//! skip this push for now" so save-flow churn doesn't drop server-side
//! state for partially-implemented edits.

use serde::{Deserialize, Serialize};

use crate::library::editing::{ColorState, CropRect, DetailState, EditState, ExposureState};

/// Image dimensions in pixels, as stored on the `media` row.
///
/// Crop coordinates are translated from EditState's normalized
/// (0.0–1.0) form into Immich's pixel coords. Rotation by 90° or 270°
/// swaps the dimensions the crop is measured against, since the local
/// renderer crops *after* rotating.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ImageDims {
    pub width: u32,
    pub height: u32,
}

/// One Immich edit action — matches the wire shape exactly.
///
/// Serializes to `{"action": "<lowercase>", "parameters": {...}}`. The
/// PUT response on `/assets/{id}/edits` echoes this same shape with an
/// added `id` field; the pull-side `SyncAssetEditV1` carries
/// `(action, parameters, sequence)` per record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", content = "parameters", rename_all = "lowercase")]
pub(crate) enum ImmichEditAction {
    Crop {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    },
    Rotate {
        angle: i32,
    },
    Mirror {
        axis: MirrorAxis,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MirrorAxis {
    Horizontal,
    Vertical,
}

/// Project an [`EditState`] to Immich's edit action list.
///
/// Returns `Some(actions)` when every operation in the state can be
/// expressed as a geometric action (the empty vec means "no edits to
/// apply"). Returns `None` when the state contains anything Immich
/// can't represent — pixel adjustments, filter presets, freeform
/// straighten — signalling to the caller that this state needs the
/// render-and-stack path (Phase C) and shouldn't touch the
/// `/edits` endpoint.
pub(crate) fn project(state: &EditState, dims: ImageDims) -> Option<Vec<ImmichEditAction>> {
    if !is_geometric_only(state) {
        return None;
    }

    let mut actions = Vec::new();
    let t = &state.transforms;

    let rot = t.rotate_degrees.rem_euclid(360);
    if rot != 0 {
        actions.push(ImmichEditAction::Rotate { angle: rot });
    }

    if t.flip_horizontal {
        actions.push(ImmichEditAction::Mirror {
            axis: MirrorAxis::Horizontal,
        });
    }
    if t.flip_vertical {
        actions.push(ImmichEditAction::Mirror {
            axis: MirrorAxis::Vertical,
        });
    }

    if let Some(crop) = &t.crop {
        // Crop is normalized against the *post-rotate* image. A 90°/270°
        // rotation swaps the basis; mirrors don't.
        let (basis_w, basis_h) = if matches!(rot, 90 | 270) {
            (dims.height, dims.width)
        } else {
            (dims.width, dims.height)
        };
        let x = (crop.x * basis_w as f64).round().max(0.0) as i32;
        let y = (crop.y * basis_h as f64).round().max(0.0) as i32;
        let width = (crop.width * basis_w as f64).round().max(1.0) as i32;
        let height = (crop.height * basis_h as f64).round().max(1.0) as i32;
        actions.push(ImmichEditAction::Crop {
            x,
            y,
            width,
            height,
        });
    }

    Some(actions)
}

/// Fold a sequence-ordered list of [`ImmichEditAction`]s back into an
/// [`EditState`]. Inverse of [`project`] for the canonical shape we
/// produce ourselves (rotate → mirrors → crop). Server-side action
/// lists from third-party tools may use other shapes — this fold
/// degrades reasonably:
///
/// - Multiple `Rotate` actions sum modulo 360.
/// - Repeated `Mirror` of the same axis cancels (XOR).
/// - The last `Crop` wins; intermediate crops are conservatively
///   ignored (the local schema only carries one crop, and our renderer
///   applies crop after rotate+flip).
///
/// `dims` is the original image's pixel dimensions and is needed to
/// normalise the crop. Rotates by 90°/270° swap the basis the crop
/// is measured against — this matches `project`'s forward path.
pub(crate) fn recompose(actions: &[ImmichEditAction], dims: ImageDims) -> EditState {
    let mut state = EditState::default();
    let mut rot: i32 = 0;

    for action in actions {
        match action {
            ImmichEditAction::Rotate { angle } => {
                rot = (rot + angle).rem_euclid(360);
                state.transforms.rotate_degrees = rot;
            }
            ImmichEditAction::Mirror {
                axis: MirrorAxis::Horizontal,
            } => {
                state.transforms.flip_horizontal ^= true;
            }
            ImmichEditAction::Mirror {
                axis: MirrorAxis::Vertical,
            } => {
                state.transforms.flip_vertical ^= true;
            }
            ImmichEditAction::Crop {
                x,
                y,
                width,
                height,
            } => {
                let (basis_w, basis_h) = if matches!(rot, 90 | 270) {
                    (dims.height, dims.width)
                } else {
                    (dims.width, dims.height)
                };
                if basis_w > 0 && basis_h > 0 {
                    state.transforms.crop = Some(CropRect {
                        x: *x as f64 / basis_w as f64,
                        y: *y as f64 / basis_h as f64,
                        width: *width as f64 / basis_w as f64,
                        height: *height as f64 / basis_h as f64,
                    });
                }
            }
        }
    }

    state
}

/// True if the state contains nothing Immich's geometric edit list
/// can't represent. Mirrors `project()`'s gate so callers can ask the
/// question without producing the action list.
fn is_geometric_only(state: &EditState) -> bool {
    state.exposure == ExposureState::default()
        && state.color == ColorState::default()
        && state.detail == DetailState::default()
        && state.filter.is_none()
        && state.transforms.straighten_degrees == 0.0
        && {
            // Rotate must be one of {0, 90, 180, 270} after normalisation.
            // The schema permits arbitrary i32; reject anything else so we
            // don't produce garbage rotations on the server.
            let rot = state.transforms.rotate_degrees.rem_euclid(360);
            matches!(rot, 0 | 90 | 180 | 270)
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::editing::CropRect;

    fn dims() -> ImageDims {
        ImageDims {
            width: 1000,
            height: 500,
        }
    }

    #[test]
    fn identity_state_projects_to_empty_vec() {
        let actions = project(&EditState::default(), dims()).unwrap();
        assert!(actions.is_empty());
    }

    #[test]
    fn pixel_adjustments_are_not_projectable() {
        let mut state = EditState::default();
        state.exposure.brightness = 0.3;
        assert!(project(&state, dims()).is_none());
    }

    #[test]
    fn filter_preset_is_not_projectable() {
        let state = EditState {
            filter: Some("vintage".into()),
            ..Default::default()
        };
        assert!(project(&state, dims()).is_none());
    }

    #[test]
    fn straighten_is_not_projectable() {
        // Freeform straighten can't be expressed by Immich's `rotate`
        // action (which only takes 90° increments).
        let mut state = EditState::default();
        state.transforms.straighten_degrees = 2.5;
        assert!(project(&state, dims()).is_none());
    }

    #[test]
    fn rotate_90_serialises_to_wire_shape() {
        let mut state = EditState::default();
        state.transforms.rotate_degrees = 90;
        let actions = project(&state, dims()).unwrap();
        assert_eq!(actions, vec![ImmichEditAction::Rotate { angle: 90 }]);
        let json = serde_json::to_value(&actions[0]).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"action": "rotate", "parameters": {"angle": 90}})
        );
    }

    #[test]
    fn flip_emits_mirror_actions() {
        let mut state = EditState::default();
        state.transforms.flip_horizontal = true;
        state.transforms.flip_vertical = true;
        let actions = project(&state, dims()).unwrap();
        assert_eq!(
            actions,
            vec![
                ImmichEditAction::Mirror {
                    axis: MirrorAxis::Horizontal
                },
                ImmichEditAction::Mirror {
                    axis: MirrorAxis::Vertical
                },
            ]
        );
    }

    #[test]
    fn crop_translates_normalized_to_pixels() {
        let mut state = EditState::default();
        state.transforms.crop = Some(CropRect {
            x: 0.1,
            y: 0.2,
            width: 0.5,
            height: 0.4,
        });
        let actions = project(&state, dims()).unwrap();
        // dims = 1000x500
        assert_eq!(
            actions,
            vec![ImmichEditAction::Crop {
                x: 100,
                y: 100,
                width: 500,
                height: 200,
            }]
        );
    }

    #[test]
    fn crop_after_rotate_uses_swapped_basis() {
        // 1000x500 rotated 90° → 500x1000. A normalized crop in that
        // post-rotate frame must translate against the swapped dims.
        let mut state = EditState::default();
        state.transforms.rotate_degrees = 90;
        state.transforms.crop = Some(CropRect {
            x: 0.0,
            y: 0.0,
            width: 0.5,
            height: 0.5,
        });
        let actions = project(&state, dims()).unwrap();
        let crop = actions
            .iter()
            .find_map(|a| match a {
                ImmichEditAction::Crop {
                    x,
                    y,
                    width,
                    height,
                } => Some((*x, *y, *width, *height)),
                _ => None,
            })
            .unwrap();
        // basis_w = 500 (orig height), basis_h = 1000 (orig width)
        assert_eq!(crop, (0, 0, 250, 500));
    }

    #[test]
    fn full_pipeline_emits_rotate_then_mirror_then_crop() {
        let mut state = EditState::default();
        state.transforms.rotate_degrees = 180;
        state.transforms.flip_horizontal = true;
        state.transforms.crop = Some(CropRect {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        });
        let actions = project(&state, dims()).unwrap();
        assert!(matches!(actions[0], ImmichEditAction::Rotate { .. }));
        assert!(matches!(actions[1], ImmichEditAction::Mirror { .. }));
        assert!(matches!(actions[2], ImmichEditAction::Crop { .. }));
    }

    #[test]
    fn rotate_degrees_normalised_to_90s() {
        // 450° normalises to 90°; -90° normalises to 270°.
        for (input, expected) in [(450, 90), (-90, 270), (720, 0)] {
            let mut state = EditState::default();
            state.transforms.rotate_degrees = input;
            let actions = project(&state, dims()).unwrap();
            if expected == 0 {
                assert!(actions.is_empty(), "no rotate emitted for input {input}");
            } else {
                assert_eq!(
                    actions[0],
                    ImmichEditAction::Rotate { angle: expected },
                    "input {input}"
                );
            }
        }
    }

    #[test]
    fn rotate_off_axis_is_not_projectable() {
        // Schema permits arbitrary i32 in `rotate_degrees`. Reject
        // values that don't normalise to a 90° increment.
        let mut state = EditState::default();
        state.transforms.rotate_degrees = 45;
        assert!(project(&state, dims()).is_none());
    }

    #[test]
    fn mirror_axis_serialises_lowercase() {
        let action = ImmichEditAction::Mirror {
            axis: MirrorAxis::Horizontal,
        };
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"action": "mirror", "parameters": {"axis": "horizontal"}})
        );
    }

    #[test]
    fn project_then_recompose_round_trips() {
        let mut state = EditState::default();
        state.transforms.rotate_degrees = 90;
        state.transforms.flip_horizontal = true;
        state.transforms.crop = Some(CropRect {
            x: 0.1,
            y: 0.2,
            width: 0.5,
            height: 0.4,
        });

        let actions = project(&state, dims()).unwrap();
        let recomposed = recompose(&actions, dims());

        assert_eq!(recomposed.transforms.rotate_degrees, 90);
        assert!(recomposed.transforms.flip_horizontal);
        let crop = recomposed.transforms.crop.unwrap();
        // Float round-trip via i32 pixel coords — accept ε.
        assert!((crop.x - 0.1).abs() < 1e-3);
        assert!((crop.y - 0.2).abs() < 1e-3);
        assert!((crop.width - 0.5).abs() < 1e-3);
        assert!((crop.height - 0.4).abs() < 1e-3);
    }

    #[test]
    fn recompose_empty_action_list_is_identity() {
        let state = recompose(&[], dims());
        assert!(state.is_identity());
    }

    #[test]
    fn recompose_rotates_sum_modulo_360() {
        let actions = vec![
            ImmichEditAction::Rotate { angle: 90 },
            ImmichEditAction::Rotate { angle: 270 },
        ];
        let state = recompose(&actions, dims());
        assert_eq!(state.transforms.rotate_degrees, 0);
    }

    #[test]
    fn recompose_mirrors_xor() {
        // Two horizontal mirrors cancel.
        let actions = vec![
            ImmichEditAction::Mirror {
                axis: MirrorAxis::Horizontal,
            },
            ImmichEditAction::Mirror {
                axis: MirrorAxis::Horizontal,
            },
        ];
        let state = recompose(&actions, dims());
        assert!(!state.transforms.flip_horizontal);
    }

    #[test]
    fn recompose_last_crop_wins() {
        // Two crops in succession — local schema only carries one.
        let actions = vec![
            ImmichEditAction::Crop {
                x: 0,
                y: 0,
                width: 500,
                height: 250,
            },
            ImmichEditAction::Crop {
                x: 100,
                y: 100,
                width: 200,
                height: 100,
            },
        ];
        let state = recompose(&actions, dims());
        let crop = state.transforms.crop.unwrap();
        // Last action's coords against unrotated basis (1000x500).
        assert!((crop.x - 0.1).abs() < 1e-9);
        assert!((crop.width - 0.2).abs() < 1e-9);
    }

    #[test]
    fn deserialises_wire_shape() {
        // Pull side: the sync stream emits one of these per
        // SyncAssetEditV1.parameters.
        let json = serde_json::json!({
            "action": "crop",
            "parameters": {"x": 10, "y": 20, "width": 100, "height": 200}
        });
        let action: ImmichEditAction = serde_json::from_value(json).unwrap();
        assert_eq!(
            action,
            ImmichEditAction::Crop {
                x: 10,
                y: 20,
                width: 100,
                height: 200,
            }
        );
    }
}
