use image::{DynamicImage, GenericImageView, Rgba};

use super::editing::{ColorState, EditState, ExposureState, TransformState};

/// Apply all edit operations to an image and return the result.
///
/// Operations are applied in a fixed order:
/// 1. Geometric transforms (rotate 90° steps, flip, straighten)
/// 2. Crop (normalized coordinates → pixel coordinates)
/// 3. Exposure adjustments (brightness, contrast, highlights, shadows, white balance)
/// 4. Color adjustments (saturation, vibrance, hue shift, temperature, tint)
///
/// This is a pure function with no I/O — suitable for calling on a blocking thread.
pub fn apply_edits(img: DynamicImage, state: &EditState) -> DynamicImage {
    if state.is_identity() {
        return img;
    }

    let img = apply_transforms(img, &state.transforms);
    let img = apply_exposure(img, &state.exposure);
    apply_color(img, &state.color)
}

// ---------------------------------------------------------------------------
// Geometric transforms
// ---------------------------------------------------------------------------

fn apply_transforms(img: DynamicImage, t: &TransformState) -> DynamicImage {
    // Rotate in 90-degree steps.
    let mut img = match t.rotate_degrees.rem_euclid(360) {
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        _ => img,
    };

    // Flip.
    if t.flip_horizontal {
        img = img.fliph();
    }
    if t.flip_vertical {
        img = img.flipv();
    }

    // Straighten (freeform rotation) — deferred to Phase 4 (#222).
    // Requires affine rotation with bilinear interpolation.

    // Crop using normalized coordinates.
    if let Some(ref crop) = t.crop {
        let (w, h) = img.dimensions();
        let cx = (crop.x * w as f64).round() as u32;
        let cy = (crop.y * h as f64).round() as u32;
        let cw = (crop.width * w as f64).round().max(1.0) as u32;
        let ch = (crop.height * h as f64).round().max(1.0) as u32;
        // Clamp to image bounds.
        let cw = cw.min(w.saturating_sub(cx));
        let ch = ch.min(h.saturating_sub(cy));
        if cw > 0 && ch > 0 {
            img = img.crop_imm(cx, cy, cw, ch);
        }
    }

    img
}

// ---------------------------------------------------------------------------
// Exposure adjustments
// ---------------------------------------------------------------------------

fn apply_exposure(img: DynamicImage, e: &ExposureState) -> DynamicImage {
    if *e == ExposureState::default() {
        return img;
    }

    let mut rgba = img.into_rgba8();
    let (w, h) = rgba.dimensions();

    for y in 0..h {
        for x in 0..w {
            let px = rgba.get_pixel(x, y);
            let [r, g, b, a] = px.0;

            // Work in 0.0–1.0 linear space.
            let mut rf = r as f64 / 255.0;
            let mut gf = g as f64 / 255.0;
            let mut bf = b as f64 / 255.0;

            // Brightness: shift all channels.
            rf += e.brightness;
            gf += e.brightness;
            bf += e.brightness;

            // Contrast: scale around midpoint (0.5).
            let factor = 1.0 + e.contrast;
            rf = (rf - 0.5) * factor + 0.5;
            gf = (gf - 0.5) * factor + 0.5;
            bf = (bf - 0.5) * factor + 0.5;

            // Highlights: boost bright pixels (luminance > 0.5).
            // Shadows: boost dark pixels (luminance < 0.5).
            let lum = 0.299 * rf + 0.587 * gf + 0.114 * bf;
            if lum > 0.5 {
                let t = (lum - 0.5) * 2.0; // 0..1 for bright pixels
                rf += e.highlights * t * 0.3;
                gf += e.highlights * t * 0.3;
                bf += e.highlights * t * 0.3;
            } else {
                let t = (0.5 - lum) * 2.0; // 0..1 for dark pixels
                rf += e.shadows * t * 0.3;
                gf += e.shadows * t * 0.3;
                bf += e.shadows * t * 0.3;
            }

            // White balance: shift blue-yellow axis.
            rf += e.white_balance * 0.1;
            gf += e.white_balance * 0.05;
            bf -= e.white_balance * 0.1;

            rgba.put_pixel(x, y, Rgba([
                clamp_u8(rf),
                clamp_u8(gf),
                clamp_u8(bf),
                a,
            ]));
        }
    }

    DynamicImage::ImageRgba8(rgba)
}

// ---------------------------------------------------------------------------
// Color adjustments
// ---------------------------------------------------------------------------

fn apply_color(img: DynamicImage, c: &ColorState) -> DynamicImage {
    if *c == ColorState::default() {
        return img;
    }

    let mut rgba = img.into_rgba8();
    let (w, h) = rgba.dimensions();

    for y in 0..h {
        for x in 0..w {
            let px = rgba.get_pixel(x, y);
            let [r, g, b, a] = px.0;

            let rf = r as f64 / 255.0;
            let gf = g as f64 / 255.0;
            let bf = b as f64 / 255.0;

            let (mut h, mut s, l) = rgb_to_hsl(rf, gf, bf);

            // Saturation: scale S channel.
            s = (s + c.saturation * s.max(0.1)).clamp(0.0, 1.0);

            // Vibrance: like saturation but less effect on already-saturated colors.
            let vibrance_scale = 1.0 - s; // low-sat pixels get more boost
            s = (s + c.vibrance * vibrance_scale * 0.5).clamp(0.0, 1.0);

            // Hue shift: rotate H.
            h = (h + c.hue_shift * 180.0).rem_euclid(360.0);

            let (mut rf, mut gf, mut bf) = hsl_to_rgb(h, s, l);

            // Temperature: warm (positive) shifts toward yellow, cool toward blue.
            rf += c.temperature * 0.1;
            gf += c.temperature * 0.05;
            bf -= c.temperature * 0.1;

            // Tint: shift green-magenta axis.
            gf += c.tint * 0.1;
            rf -= c.tint * 0.05;
            bf -= c.tint * 0.05;

            rgba.put_pixel(x, y, Rgba([
                clamp_u8(rf),
                clamp_u8(gf),
                clamp_u8(bf),
                a,
            ]));
        }
    }

    DynamicImage::ImageRgba8(rgba)
}

// ---------------------------------------------------------------------------
// Filter presets
// ---------------------------------------------------------------------------

/// Built-in filter presets. Each preset is an `EditState` with exposure and
/// color values tuned for a particular look.
pub fn filter_preset(name: &str) -> Option<EditState> {
    let mut state = EditState::default();
    state.filter = Some(name.to_string());

    match name {
        "bw" => {
            state.color.saturation = -1.0;
            state.exposure.contrast = 0.1;
        }
        "vintage" => {
            state.color.saturation = -0.3;
            state.color.temperature = 0.3;
            state.exposure.contrast = -0.1;
            state.exposure.brightness = 0.05;
        }
        "warm" => {
            state.color.temperature = 0.4;
            state.color.saturation = 0.1;
        }
        "cool" => {
            state.color.temperature = -0.4;
            state.color.saturation = 0.1;
        }
        "vivid" => {
            state.color.saturation = 0.5;
            state.color.vibrance = 0.3;
            state.exposure.contrast = 0.15;
        }
        "fade" => {
            state.exposure.contrast = -0.2;
            state.exposure.brightness = 0.1;
            state.color.saturation = -0.2;
        }
        _ => return None,
    }

    Some(state)
}

/// Names of all built-in filter presets.
pub const FILTER_NAMES: &[&str] = &["bw", "vintage", "warm", "cool", "vivid", "fade"];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn clamp_u8(v: f64) -> u8 {
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Convert RGB (0.0–1.0) to HSL (H: 0–360, S: 0–1, L: 0–1).
fn rgb_to_hsl(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;

    if (max - min).abs() < f64::EPSILON {
        return (0.0, 0.0, l);
    }

    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };

    let h = if (max - r).abs() < f64::EPSILON {
        ((g - b) / d).rem_euclid(6.0)
    } else if (max - g).abs() < f64::EPSILON {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };

    (h * 60.0, s, l)
}

/// Convert HSL (H: 0–360, S: 0–1, L: 0–1) to RGB (0.0–1.0).
fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (f64, f64, f64) {
    if s.abs() < f64::EPSILON {
        return (l, l, l);
    }

    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let h_norm = h / 360.0;

    let r = hue_to_rgb(p, q, h_norm + 1.0 / 3.0);
    let g = hue_to_rgb(p, q, h_norm);
    let b = hue_to_rgb(p, q, h_norm - 1.0 / 3.0);

    (r, g, b)
}

fn hue_to_rgb(p: f64, q: f64, mut t: f64) -> f64 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 1.0 / 2.0 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::editing::CropRect;
    use image::RgbaImage;

    fn test_image() -> DynamicImage {
        // 4x4 red image for fast tests.
        let mut img = RgbaImage::new(4, 4);
        for px in img.pixels_mut() {
            *px = Rgba([200, 100, 50, 255]);
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn identity_is_noop() {
        let img = test_image();
        let result = apply_edits(img.clone(), &EditState::default());
        assert_eq!(img.dimensions(), result.dimensions());
        assert_eq!(img.as_rgba8().unwrap().as_raw(), result.as_rgba8().unwrap().as_raw());
    }

    #[test]
    fn rotate_90_swaps_dimensions() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(4, 2));
        let mut state = EditState::default();
        state.transforms.rotate_degrees = 90;
        let result = apply_edits(img, &state);
        assert_eq!(result.dimensions(), (2, 4));
    }

    #[test]
    fn rotate_180_preserves_dimensions() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(4, 2));
        let mut state = EditState::default();
        state.transforms.rotate_degrees = 180;
        let result = apply_edits(img, &state);
        assert_eq!(result.dimensions(), (4, 2));
    }

    #[test]
    fn flip_horizontal() {
        let mut img = RgbaImage::new(2, 1);
        img.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        img.put_pixel(1, 0, Rgba([0, 255, 0, 255]));
        let img = DynamicImage::ImageRgba8(img);

        let mut state = EditState::default();
        state.transforms.flip_horizontal = true;
        let result = apply_edits(img, &state);
        let rgba = result.as_rgba8().unwrap();
        assert_eq!(rgba.get_pixel(0, 0).0, [0, 255, 0, 255]);
        assert_eq!(rgba.get_pixel(1, 0).0, [255, 0, 0, 255]);
    }

    #[test]
    fn crop_halves_image() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(100, 100));
        let mut state = EditState::default();
        state.transforms.crop = Some(CropRect {
            x: 0.0,
            y: 0.0,
            width: 0.5,
            height: 0.5,
        });
        let result = apply_edits(img, &state);
        assert_eq!(result.dimensions(), (50, 50));
    }

    #[test]
    fn brightness_increases_pixel_values() {
        let img = test_image();
        let mut state = EditState::default();
        state.exposure.brightness = 0.5;
        let result = apply_edits(img.clone(), &state);

        let orig_px = img.as_rgba8().unwrap().get_pixel(0, 0);
        let edit_px = result.as_rgba8().unwrap().get_pixel(0, 0);
        assert!(edit_px[0] > orig_px[0]);
        assert!(edit_px[1] > orig_px[1]);
        assert!(edit_px[2] > orig_px[2]);
    }

    #[test]
    fn negative_brightness_decreases_pixel_values() {
        let img = test_image();
        let mut state = EditState::default();
        state.exposure.brightness = -0.5;
        let result = apply_edits(img.clone(), &state);

        let orig_px = img.as_rgba8().unwrap().get_pixel(0, 0);
        let edit_px = result.as_rgba8().unwrap().get_pixel(0, 0);
        assert!(edit_px[0] < orig_px[0]);
    }

    #[test]
    fn full_desaturation_produces_grayscale() {
        let img = test_image();
        let mut state = EditState::default();
        state.color.saturation = -1.0;
        let result = apply_edits(img, &state);

        let px = result.as_rgba8().unwrap().get_pixel(0, 0);
        // In grayscale, R ≈ G ≈ B (may differ by 1 due to rounding).
        assert!((px[0] as i16 - px[1] as i16).abs() <= 1);
        assert!((px[1] as i16 - px[2] as i16).abs() <= 1);
    }

    #[test]
    fn bw_filter_desaturates() {
        let preset = filter_preset("bw").unwrap();
        assert_eq!(preset.color.saturation, -1.0);
        assert!(preset.filter.as_deref() == Some("bw"));
    }

    #[test]
    fn all_filter_presets_are_valid() {
        for name in FILTER_NAMES {
            let preset = filter_preset(name);
            assert!(preset.is_some(), "filter '{name}' should exist");
            assert!(!preset.unwrap().is_identity(), "filter '{name}' should not be identity");
        }
    }

    #[test]
    fn unknown_filter_returns_none() {
        assert!(filter_preset("nonexistent").is_none());
    }

    #[test]
    fn hsl_round_trip() {
        let (r, g, b) = (0.8, 0.4, 0.2);
        let (h, s, l) = rgb_to_hsl(r, g, b);
        let (r2, g2, b2) = hsl_to_rgb(h, s, l);
        assert!((r - r2).abs() < 0.01);
        assert!((g - g2).abs() < 0.01);
        assert!((b - b2).abs() < 0.01);
    }

    #[test]
    fn combined_edits_apply_in_order() {
        let img = DynamicImage::ImageRgba8(RgbaImage::new(100, 50));
        let mut state = EditState::default();
        // Rotate 90 (100x50 → 50x100), then crop to left half.
        state.transforms.rotate_degrees = 90;
        state.transforms.crop = Some(CropRect {
            x: 0.0,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        });
        let result = apply_edits(img, &state);
        assert_eq!(result.dimensions(), (25, 100));
    }

    #[test]
    fn alpha_channel_preserved() {
        let mut img = RgbaImage::new(2, 2);
        for px in img.pixels_mut() {
            *px = Rgba([100, 100, 100, 128]);
        }
        let img = DynamicImage::ImageRgba8(img);
        let mut state = EditState::default();
        state.exposure.brightness = 0.5;
        state.color.saturation = 0.5;
        let result = apply_edits(img, &state);
        let px = result.as_rgba8().unwrap().get_pixel(0, 0);
        assert_eq!(px[3], 128); // Alpha unchanged
    }
}
