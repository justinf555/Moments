// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::Path;

use tracing::{instrument, warn};

/// All EXIF fields extracted from a single image file.
///
/// Every field is `Option` — missing or unreadable EXIF never fails the
/// import pipeline. Callers must handle absent data gracefully.
#[derive(Debug, Default)]
pub struct ExifInfo {
    /// Capture time as the *capture-local wall clock*, encoded as seconds
    /// since the epoch (the EXIF digits read as if they were UTC).
    ///
    /// This deliberately is not an instant in UTC. EXIF `DateTimeOriginal`
    /// records what the camera's clock read, and photo libraries show that
    /// same reading no matter where the viewer later opens the file — a
    /// sunset shot at 18:04 in Sydney stays "18:04", not "08:04" because
    /// the laptop is now in London. Immich uses the same convention for
    /// its `localDateTime` field, which is what the sync handler stores
    /// into `media.taken_at`, so both import paths agree. Issue #549.
    pub captured_at: Option<i64>,
    /// UTC offset of the capture timezone, in minutes east of UTC, from
    /// the `OffsetTimeOriginal` / `OffsetTime` tags. Recorded for future
    /// use (it is not currently persisted) and intentionally *not* applied
    /// to [`Self::captured_at`].
    pub captured_at_tz: Option<i64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// EXIF orientation tag (1–8).
    pub orientation: Option<u8>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    /// Aperture as an F-number (e.g. `2.8`).
    pub aperture: Option<f32>,
    /// Shutter speed as a human-readable string (e.g. `"1/500"` or `"2.5"`).
    pub shutter_str: Option<String>,
    pub iso: Option<u32>,
    pub focal_length: Option<f32>,
    /// GPS latitude in decimal degrees (positive = North).
    pub gps_lat: Option<f64>,
    /// GPS longitude in decimal degrees (positive = East).
    pub gps_lon: Option<f64>,
    /// GPS altitude in metres above sea level.
    pub gps_alt: Option<f64>,
    pub color_space: Option<String>,
}

/// Extract EXIF metadata from `path`.
///
/// Returns an all-`None` [`ExifInfo`] if the file has no EXIF data or if
/// parsing fails — callers never need to handle a hard error here.
///
/// Must be called from a blocking context (e.g. inside
/// [`tokio::task::spawn_blocking`]).
#[instrument(skip_all, fields(path = %path.display()))]
pub fn extract_exif(path: &Path) -> ExifInfo {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            warn!(error = %e, "could not open file for EXIF extraction");
            return ExifInfo::default();
        }
    };

    let mut reader = std::io::BufReader::new(file);
    let exif = match exif::Reader::new().read_from_container(&mut reader) {
        Ok(e) => e,
        Err(_) => return ExifInfo::default(),
    };

    let mut info = ExifInfo {
        captured_at: capture_timestamp(&exif),
        captured_at_tz: capture_tz(&exif),
        ..Default::default()
    };

    // ── Dimensions ───────────────────────────────────────────────────────────
    info.width = rational_or_short_u32(&exif, exif::Tag::PixelXDimension)
        .or_else(|| rational_or_short_u32(&exif, exif::Tag::ImageWidth));
    info.height = rational_or_short_u32(&exif, exif::Tag::PixelYDimension)
        .or_else(|| rational_or_short_u32(&exif, exif::Tag::ImageLength));

    // ── Orientation ───────────────────────────────────────────────────────────
    info.orientation = short_u8(&exif, exif::Tag::Orientation);

    // ── Camera ───────────────────────────────────────────────────────────────
    info.camera_make = ascii_string(&exif, exif::Tag::Make);
    info.camera_model = ascii_string(&exif, exif::Tag::Model);
    info.lens_model = ascii_string(&exif, exif::Tag::LensModel);

    // ── Exposure ─────────────────────────────────────────────────────────────
    info.aperture = rational_f32(&exif, exif::Tag::FNumber);
    info.shutter_str = shutter_string(&exif);
    info.iso = short_u32(&exif, exif::Tag::PhotographicSensitivity);
    info.focal_length = rational_f32(&exif, exif::Tag::FocalLength);

    // ── GPS ──────────────────────────────────────────────────────────────────
    info.gps_lat = gps_decimal(&exif, exif::Tag::GPSLatitude, exif::Tag::GPSLatitudeRef);
    info.gps_lon = gps_decimal(&exif, exif::Tag::GPSLongitude, exif::Tag::GPSLongitudeRef);
    info.gps_alt = gps_altitude(&exif);

    // ── Color space ───────────────────────────────────────────────────────────
    info.color_space = color_space_string(&exif);

    info
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn get_field(exif: &exif::Exif, tag: exif::Tag) -> Option<&exif::Field> {
    exif.get_field(tag, exif::In::PRIMARY)
        .or_else(|| exif.get_field(tag, exif::In::THUMBNAIL))
}

/// Read an ASCII EXIF field as a trimmed `String`.
fn ascii_field(exif: &exif::Exif, tag: exif::Tag) -> Option<String> {
    let field = get_field(exif, tag)?;
    match &field.value {
        exif::Value::Ascii(vecs) => {
            let s: String = vecs.first()?.iter().map(|&b| b as char).collect();
            Some(s.trim_end_matches('\0').trim().to_string())
        }
        _ => None,
    }
}

/// Parse `DateTimeOriginal` (preferred) or `DateTime` into `captured_at`.
///
/// The result is the *capture-local wall clock* encoded as seconds since
/// the epoch — i.e. the digits the camera wrote, read as if they were UTC.
/// See [`ExifInfo::captured_at`] for why the `OffsetTime*` tag is recorded
/// separately in [`ExifInfo::captured_at_tz`] rather than folded in here.
fn capture_timestamp(exif: &exif::Exif) -> Option<i64> {
    let s = ascii_field(exif, exif::Tag::DateTimeOriginal)
        .or_else(|| ascii_field(exif, exif::Tag::DateTime))?;
    parse_exif_datetime(&s)
}

/// Parse an EXIF `"YYYY:MM:DD HH:MM:SS"` string into a wall-clock timestamp.
fn parse_exif_datetime(s: &str) -> Option<i64> {
    let dt = chrono::NaiveDateTime::parse_from_str(s, "%Y:%m:%d %H:%M:%S").ok()?;
    Some(dt.and_utc().timestamp())
}

/// Parse `OffsetTimeOriginal` or `OffsetTime` into minutes east of UTC.
fn capture_tz(exif: &exif::Exif) -> Option<i64> {
    let s = ascii_field(exif, exif::Tag::OffsetTimeOriginal)
        .or_else(|| ascii_field(exif, exif::Tag::OffsetTime))?;
    parse_tz_offset(&s)
}

/// Parse an EXIF UTC-offset string (`"+HH:MM"` / `"-HH:MM"`) into minutes
/// east of UTC.
fn parse_tz_offset(s: &str) -> Option<i64> {
    let sign: i64 = if s.starts_with('-') { -1 } else { 1 };
    let s = s.trim_start_matches(['+', '-']);
    let mut parts = s.splitn(2, ':');
    let hours: i64 = parts.next()?.trim().parse().ok()?;
    let mins: i64 = parts
        .next()
        .and_then(|m| m.trim().parse::<i64>().ok())
        .unwrap_or(0);
    Some(sign * (hours * 60 + mins))
}

fn rational_or_short_u32(exif: &exif::Exif, tag: exif::Tag) -> Option<u32> {
    let field = get_field(exif, tag)?;
    match &field.value {
        exif::Value::Long(v) => v.first().copied(),
        exif::Value::Short(v) => v.first().map(|&x| x as u32),
        exif::Value::Rational(v) => v.first().map(|r| r.num.checked_div(r.denom).unwrap_or(0)),
        _ => None,
    }
}

fn short_u8(exif: &exif::Exif, tag: exif::Tag) -> Option<u8> {
    let field = get_field(exif, tag)?;
    match &field.value {
        exif::Value::Short(v) => v.first().map(|&x| x as u8),
        _ => None,
    }
}

fn short_u32(exif: &exif::Exif, tag: exif::Tag) -> Option<u32> {
    let field = get_field(exif, tag)?;
    match &field.value {
        exif::Value::Short(v) => v.first().map(|&x| x as u32),
        exif::Value::Long(v) => v.first().copied(),
        _ => None,
    }
}

fn rational_f32(exif: &exif::Exif, tag: exif::Tag) -> Option<f32> {
    let field = get_field(exif, tag)?;
    match &field.value {
        exif::Value::Rational(v) => v.first().and_then(|r| {
            if r.denom != 0 {
                Some(r.num as f32 / r.denom as f32)
            } else {
                None
            }
        }),
        _ => None,
    }
}

fn ascii_string(exif: &exif::Exif, tag: exif::Tag) -> Option<String> {
    let field = get_field(exif, tag)?;
    match &field.value {
        exif::Value::Ascii(vecs) => {
            let s: String = vecs.first()?.iter().map(|&b| b as char).collect();
            let s = s.trim_end_matches('\0').trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
        _ => None,
    }
}

/// Format `ExposureTime` rational as a human-readable shutter speed string.
fn shutter_string(exif: &exif::Exif) -> Option<String> {
    let field = get_field(exif, exif::Tag::ExposureTime)?;
    let (num, denom) = match &field.value {
        exif::Value::Rational(v) => {
            let r = v.first()?;
            (r.num, r.denom)
        }
        _ => return None,
    };

    if denom == 0 {
        return None;
    }

    if num >= denom {
        // Whole or fractional seconds: "2.0", "2.5"
        let secs = num as f32 / denom as f32;
        Some(format!("{secs:.1}"))
    } else {
        // Sub-second: simplify "10/5000" → "1/500"
        let g = gcd(num, denom);
        let n = num / g;
        let d = denom / g;
        if n == 1 {
            Some(format!("1/{d}"))
        } else {
            Some(format!("{n}/{d}"))
        }
    }
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Convert GPS DMS rational triplet + reference tag to decimal degrees.
fn gps_decimal(exif: &exif::Exif, coord_tag: exif::Tag, ref_tag: exif::Tag) -> Option<f64> {
    let coord_field = get_field(exif, coord_tag)?;
    let rationals = match &coord_field.value {
        exif::Value::Rational(v) if v.len() >= 3 => v,
        _ => return None,
    };

    let deg = rat_f64(&rationals[0]);
    let min = rat_f64(&rationals[1]);
    let sec = rat_f64(&rationals[2]);
    let decimal = deg + min / 60.0 + sec / 3600.0;

    let ref_field = get_field(exif, ref_tag)?;
    let ref_char = match &ref_field.value {
        exif::Value::Ascii(vecs) => *vecs.first()?.first()? as char,
        _ => return None,
    };

    match ref_char {
        'S' | 'W' => Some(-decimal),
        _ => Some(decimal),
    }
}

fn gps_altitude(exif: &exif::Exif) -> Option<f64> {
    let alt = get_field(exif, exif::Tag::GPSAltitude)?;
    let r = match &alt.value {
        exif::Value::Rational(v) => v.first()?,
        _ => return None,
    };
    let alt_m = rat_f64(r);

    // AltitudeRef: 0 = above sea level, 1 = below
    let below = get_field(exif, exif::Tag::GPSAltitudeRef)
        .and_then(|f| match &f.value {
            exif::Value::Byte(v) => v.first().copied(),
            _ => None,
        })
        .unwrap_or(0);

    Some(if below == 1 { -alt_m } else { alt_m })
}

fn rat_f64(r: &exif::Rational) -> f64 {
    if r.denom != 0 {
        r.num as f64 / r.denom as f64
    } else {
        0.0
    }
}

fn color_space_string(exif: &exif::Exif) -> Option<String> {
    let field = get_field(exif, exif::Tag::ColorSpace)?;
    match &field.value {
        exif::Value::Short(v) => match v.first()? {
            1 => Some("sRGB".to_string()),
            0xFFFF => Some("Uncalibrated".to_string()),
            other => Some(format!("{other}")),
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn extract_exif_on_non_image_returns_default() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"not an image").unwrap();
        let info = extract_exif(f.path());
        assert!(info.captured_at.is_none());
        assert!(info.width.is_none());
        assert!(info.camera_make.is_none());
    }

    #[test]
    fn extract_exif_on_missing_file_returns_default() {
        let info = extract_exif(Path::new("/tmp/does_not_exist_moments_test.jpg"));
        assert!(info.captured_at.is_none());
    }

    #[test]
    fn shutter_string_sub_second() {
        // Build a minimal exif with ExposureTime = 1/500
        // We can't easily inject an Exif struct, so test the helper indirectly
        // via gcd simplification
        assert_eq!(gcd(10, 5000), 10);
        // 10/5000 → 1/500
        let g = gcd(10, 5000);
        assert_eq!(10 / g, 1);
        assert_eq!(5000 / g, 500);
    }

    #[test]
    fn gcd_values() {
        assert_eq!(gcd(0, 5), 5);
        assert_eq!(gcd(12, 8), 4);
        assert_eq!(gcd(1, 500), 1);
    }

    #[test]
    fn parse_exif_datetime_keeps_wall_clock() {
        // 2024-06-15 18:04:05 must round-trip to the same digits, so the
        // info panel shows the time the shutter actually fired.
        let ts = parse_exif_datetime("2024:06:15 18:04:05").unwrap();
        let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0).unwrap();
        assert_eq!(
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2024-06-15 18:04:05"
        );
    }

    #[test]
    fn parse_exif_datetime_rejects_garbage() {
        assert!(parse_exif_datetime("").is_none());
        assert!(parse_exif_datetime("2024-06-15T18:04:05Z").is_none());
    }

    #[test]
    fn parse_tz_offset_signs_and_minutes() {
        assert_eq!(parse_tz_offset("+10:00"), Some(600));
        assert_eq!(parse_tz_offset("-05:30"), Some(-330));
        assert_eq!(parse_tz_offset("+00:00"), Some(0));
        // Some cameras omit the minutes component.
        assert_eq!(parse_tz_offset("+09"), Some(540));
    }

    #[test]
    fn parse_tz_offset_rejects_garbage() {
        assert!(parse_tz_offset("").is_none());
        assert!(parse_tz_offset("Z").is_none());
    }
}
