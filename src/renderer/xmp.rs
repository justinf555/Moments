// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! XMP encode/decode + JPEG APP1 segment injection.
//!
//! Phase C (#224) embeds a Moments-specific XMP block in every
//! rendered JPEG so a fresh-install Moments can reconstruct the local
//! `edits` row by reading the rendered sibling's bytes (Phase D).
//!
//! ## Wire shape
//!
//! ```xml
//! <x:xmpmeta xmlns:x="adobe:ns:meta/">
//!   <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
//!     <rdf:Description rdf:about=""
//!         xmlns:moments="urn:moments:edits:1.0">
//!       <moments:originalAssetId>…</moments:originalAssetId>
//!       <moments:originalContentHash>…</moments:originalContentHash>
//!       <moments:editVersion>1</moments:editVersion>
//!       <moments:renderedAt>2026-05-10T12:34:56Z</moments:renderedAt>
//!       <moments:editJson>…</moments:editJson>
//!     </rdf:Description>
//!   </rdf:RDF>
//! </x:xmpmeta>
//! ```
//!
//! ## JPEG APP1 layout
//!
//! Adobe XMP lives in an APP1 (`FF E1`) marker segment whose payload
//! starts with the marker `http://ns.adobe.com/xap/1.0/\0`. Cameras
//! commonly already use APP1 for EXIF; we walk past EXIF (and any
//! other APP segments) and insert ours right before the first non-app
//! segment, so EXIF stays intact.
//!
//! See [XMP Specification Part 3 §1.1.3](https://www.adobe.com/content/dam/acom/en/devnet/xmp/pdfs/XMP%20SDK%20Release%20cc-2016-08/XMPSpecificationPart3.pdf).

use chrono::{DateTime, Utc};
use thiserror::Error;

/// Adobe's standard XMP APP1 marker, including the trailing NUL.
pub(crate) const ADOBE_XMP_MARKER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";

/// Cap a single APP1 segment's payload at this size. JPEG segment
/// length is a 16-bit field that includes the 2 length bytes, so the
/// payload max is 65533 bytes. We refuse to inject beyond this — XMP
/// extension segments (`ExtendedXMP`) exist but Moments-edit JSON is
/// far smaller than that ceiling in practice.
const MAX_APP1_PAYLOAD: usize = 65533;

/// Phase D recovery payload — what a fresh install reads back from a
/// rendered JPEG to rebuild the local `edits` row.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedEdit {
    /// Immich asset id of the *original* (un-edited) asset. Phase D
    /// uses this to find the parent during recovery.
    pub original_asset_id: String,
    /// SHA-1 base64 of the original asset's bytes — fallback when the
    /// `original_asset_id` doesn't resolve (rare: original was
    /// re-uploaded). Matches Immich's `checksum` wire format.
    pub original_content_hash: String,
    /// Schema version of `edit_json`. Bumped only on incompatible
    /// changes; consumers parse forward-compatibly.
    pub edit_version: u32,
    /// When the render was produced locally. Tiebreaker if multiple
    /// Moments installs raced.
    pub rendered_at: DateTime<Utc>,
    /// Serialised `EditState` (the whole struct's JSON form).
    pub edit_json: String,
}

#[derive(Debug, Error)]
pub enum XmpError {
    #[error("XML parse error: {0}")]
    Xml(String),
    #[error("missing required field: {0}")]
    MissingField(&'static str),
    #[error("invalid field {field}: {reason}")]
    InvalidField { field: &'static str, reason: String },
    #[error("JPEG parse error: {0}")]
    Jpeg(&'static str),
    #[error("XMP block too large for one APP1 segment ({size} > {max})")]
    TooLarge { size: usize, max: usize },
}

// ── XMP encode (hand-rolled, deterministic output) ────────────────────

/// Serialise an [`EmbeddedEdit`] into the canonical XMP block.
pub fn encode(edit: &EmbeddedEdit) -> String {
    let mut s = String::with_capacity(512 + edit.edit_json.len());
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">");
    s.push_str("<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">");
    s.push_str("<rdf:Description rdf:about=\"\" xmlns:moments=\"urn:moments:edits:1.0\">");
    push_element(&mut s, "moments:originalAssetId", &edit.original_asset_id);
    push_element(
        &mut s,
        "moments:originalContentHash",
        &edit.original_content_hash,
    );
    push_element(
        &mut s,
        "moments:editVersion",
        &edit.edit_version.to_string(),
    );
    push_element(&mut s, "moments:renderedAt", &edit.rendered_at.to_rfc3339());
    push_element(&mut s, "moments:editJson", &edit.edit_json);
    s.push_str("</rdf:Description>");
    s.push_str("</rdf:RDF>");
    s.push_str("</x:xmpmeta>");
    s
}

fn push_element(buf: &mut String, tag: &str, content: &str) {
    buf.push('<');
    buf.push_str(tag);
    buf.push('>');
    push_xml_escaped(buf, content);
    buf.push_str("</");
    buf.push_str(tag);
    buf.push('>');
}

/// Resolve the five XML named entities + numeric character references.
/// Returns `None` for unknown entity names so the caller can decide
/// whether to keep the raw text or fail.
fn resolve_xml_entity(name: &str) -> Option<String> {
    match name {
        "amp" => Some("&".into()),
        "lt" => Some("<".into()),
        "gt" => Some(">".into()),
        "quot" => Some("\"".into()),
        "apos" => Some("'".into()),
        n if n.starts_with('#') => {
            let digits = &n[1..];
            let code = if let Some(hex) = digits
                .strip_prefix('x')
                .or_else(|| digits.strip_prefix('X'))
            {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                digits.parse::<u32>().ok()?
            };
            char::from_u32(code).map(|c| c.to_string())
        }
        _ => None,
    }
}

fn push_xml_escaped(buf: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => buf.push_str("&amp;"),
            '<' => buf.push_str("&lt;"),
            '>' => buf.push_str("&gt;"),
            '"' => buf.push_str("&quot;"),
            '\'' => buf.push_str("&apos;"),
            _ => buf.push(c),
        }
    }
}

// ── XMP decode (quick-xml — survives whitespace/attribute drift) ──────

/// Parse an XMP block back into an [`EmbeddedEdit`].
pub fn decode(xml: &str) -> Result<EmbeddedEdit, XmpError> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    let mut original_asset_id: Option<String> = None;
    let mut original_content_hash: Option<String> = None;
    let mut edit_version: Option<u32> = None;
    let mut rendered_at: Option<DateTime<Utc>> = None;
    let mut edit_json: Option<String> = None;

    // Accumulate all text+CDATA inside a `moments:` element and only
    // commit on End — quick-xml splits text runs around entity refs,
    // and a JSON payload with escaped quotes would be lost otherwise.
    let mut buf = Vec::new();
    let mut current_field: Option<&'static str> = None;
    let mut accumulator = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Err(e) => {
                return Err(XmpError::Xml(format!(
                    "at {}: {e}",
                    reader.error_position()
                )))
            }
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                let name = e.name();
                let local = std::str::from_utf8(name.local_name().as_ref())
                    .unwrap_or("")
                    .to_string();
                let new_field = match local.as_str() {
                    "originalAssetId" => Some("originalAssetId"),
                    "originalContentHash" => Some("originalContentHash"),
                    "editVersion" => Some("editVersion"),
                    "renderedAt" => Some("renderedAt"),
                    "editJson" => Some("editJson"),
                    _ => None,
                };
                if new_field.is_some() {
                    accumulator.clear();
                    current_field = new_field;
                }
            }
            Ok(Event::End(_)) => {
                if let Some(field) = current_field.take() {
                    let value = std::mem::take(&mut accumulator);
                    match field {
                        "originalAssetId" => original_asset_id = Some(value),
                        "originalContentHash" => original_content_hash = Some(value),
                        "editVersion" => {
                            edit_version = Some(value.trim().parse().map_err(
                                |e: std::num::ParseIntError| XmpError::InvalidField {
                                    field: "editVersion",
                                    reason: e.to_string(),
                                },
                            )?);
                        }
                        "renderedAt" => {
                            rendered_at = Some(
                                DateTime::parse_from_rfc3339(value.trim())
                                    .map_err(|e| XmpError::InvalidField {
                                        field: "renderedAt",
                                        reason: e.to_string(),
                                    })?
                                    .with_timezone(&Utc),
                            );
                        }
                        "editJson" => edit_json = Some(value),
                        _ => {}
                    }
                }
            }
            Ok(Event::Text(t)) if current_field.is_some() => {
                let decoded = t.decode().map_err(|e| XmpError::Xml(e.to_string()))?;
                let unescaped = quick_xml::escape::unescape(&decoded)
                    .map_err(|e| XmpError::Xml(e.to_string()))?;
                accumulator.push_str(&unescaped);
            }
            Ok(Event::CData(t)) if current_field.is_some() => {
                // CDATA is literal — no entity unescaping.
                let s = std::str::from_utf8(&t)
                    .map_err(|e| XmpError::Xml(format!("CDATA not UTF-8: {e}")))?;
                accumulator.push_str(s);
            }
            // quick-xml emits entity references (`&quot;`, `&#34;`, etc.)
            // as their own events, separate from surrounding Text. We
            // resolve standard XML entities and numeric refs in-line so
            // the accumulator sees the original text. Anything we don't
            // recognise is kept as `&name;` rather than being dropped —
            // that round-trips through the encode-side escaping.
            Ok(Event::GeneralRef(r)) if current_field.is_some() => {
                let name = r.decode().map_err(|e| XmpError::Xml(e.to_string()))?;
                if let Some(resolved) = resolve_xml_entity(&name) {
                    accumulator.push_str(&resolved);
                } else {
                    accumulator.push('&');
                    accumulator.push_str(&name);
                    accumulator.push(';');
                }
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(EmbeddedEdit {
        original_asset_id: original_asset_id.ok_or(XmpError::MissingField("originalAssetId"))?,
        original_content_hash: original_content_hash
            .ok_or(XmpError::MissingField("originalContentHash"))?,
        edit_version: edit_version.ok_or(XmpError::MissingField("editVersion"))?,
        rendered_at: rendered_at.ok_or(XmpError::MissingField("renderedAt"))?,
        edit_json: edit_json.ok_or(XmpError::MissingField("editJson"))?,
    })
}

// ── JPEG APP1 segment injection ───────────────────────────────────────

/// Append an Adobe XMP APP1 segment to an in-memory JPEG.
///
/// The segment is inserted **after** any existing APP segments (so an
/// EXIF APP1 stays intact) and before the first non-app segment.
/// Returns `XmpError::Jpeg` if the input doesn't start with SOI or
/// runs out of segments before SOS.
pub fn inject_xmp(jpeg: &[u8], xmp: &str) -> Result<Vec<u8>, XmpError> {
    let xmp_bytes = xmp.as_bytes();
    let segment_payload_len = ADOBE_XMP_MARKER.len() + xmp_bytes.len();
    if segment_payload_len > MAX_APP1_PAYLOAD {
        return Err(XmpError::TooLarge {
            size: segment_payload_len,
            max: MAX_APP1_PAYLOAD,
        });
    }

    let insert_at = find_post_app_offset(jpeg)?;

    // segment len = 2 (length field itself) + marker + xmp
    let seg_len = 2 + segment_payload_len;
    let mut out = Vec::with_capacity(jpeg.len() + 4 + segment_payload_len);
    out.extend_from_slice(&jpeg[..insert_at]);
    out.push(0xFF);
    out.push(0xE1);
    out.extend_from_slice(&(seg_len as u16).to_be_bytes());
    out.extend_from_slice(ADOBE_XMP_MARKER);
    out.extend_from_slice(xmp_bytes);
    out.extend_from_slice(&jpeg[insert_at..]);
    Ok(out)
}

/// Walk the JPEG header marker chain and return the byte offset of
/// the first non-app segment (DQT, SOF, etc.) — i.e. the right
/// insertion point for a new APP marker.
///
/// JPEG layout:
///   * `FF D8`       — SOI (no length)
///   * `FF E0..EF`   — APP0..APP15: `FF Ex LL LL [data...]`, length is BE u16 *including* the 2 length bytes
///   * `FF DB / C0 / DA / …` — DQT / SOF / SOS / etc.
fn find_post_app_offset(jpeg: &[u8]) -> Result<usize, XmpError> {
    if jpeg.len() < 4 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
        return Err(XmpError::Jpeg("missing SOI marker"));
    }
    let mut i = 2;
    loop {
        if i + 1 >= jpeg.len() {
            return Err(XmpError::Jpeg("truncated before any non-SOI segment"));
        }
        if jpeg[i] != 0xFF {
            return Err(XmpError::Jpeg(
                "unexpected byte where marker prefix expected",
            ));
        }
        // Skip over consecutive 0xFF fill bytes (rare but legal).
        while i < jpeg.len() && jpeg[i] == 0xFF {
            i += 1;
        }
        if i >= jpeg.len() {
            return Err(XmpError::Jpeg("truncated after FF padding"));
        }
        let marker = jpeg[i];
        i += 1;
        // APP markers (0xE0..=0xEF) — keep walking past these
        if (0xE0..=0xEF).contains(&marker) {
            if i + 2 > jpeg.len() {
                return Err(XmpError::Jpeg("truncated APP segment length"));
            }
            let seg_len = u16::from_be_bytes([jpeg[i], jpeg[i + 1]]) as usize;
            if seg_len < 2 || i + seg_len > jpeg.len() {
                return Err(XmpError::Jpeg("APP segment length out of bounds"));
            }
            i += seg_len;
            continue;
        }
        // Any other marker: insertion point is just before the FF prefix,
        // i.e. before the marker byte we already advanced past — back up
        // to the FF (skipping any fill 0xFFs we walked through).
        let mut back = i - 1; // the marker byte
        while back > 0 && jpeg[back - 1] == 0xFF {
            back -= 1;
        }
        return Ok(back);
    }
}

/// Find and return the XMP block embedded in a JPEG, if any.
///
/// Walks the APP1 segments and returns the first whose payload starts
/// with the Adobe XMP marker. Returns `None` if no such segment exists.
pub fn extract_xmp(jpeg: &[u8]) -> Result<Option<String>, XmpError> {
    if jpeg.len() < 4 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
        return Err(XmpError::Jpeg("missing SOI marker"));
    }
    let mut i = 2;
    loop {
        if i + 1 >= jpeg.len() {
            return Ok(None);
        }
        while i < jpeg.len() && jpeg[i] == 0xFF {
            i += 1;
        }
        if i >= jpeg.len() {
            return Ok(None);
        }
        let marker = jpeg[i];
        i += 1;
        if (0xE0..=0xEF).contains(&marker) {
            if i + 2 > jpeg.len() {
                return Ok(None);
            }
            let seg_len = u16::from_be_bytes([jpeg[i], jpeg[i + 1]]) as usize;
            if seg_len < 2 || i + seg_len > jpeg.len() {
                return Ok(None);
            }
            let body_start = i + 2;
            let body_end = i + seg_len;
            i = body_end;
            if marker == 0xE1
                && body_end - body_start >= ADOBE_XMP_MARKER.len()
                && &jpeg[body_start..body_start + ADOBE_XMP_MARKER.len()] == ADOBE_XMP_MARKER
            {
                let xmp_bytes = &jpeg[body_start + ADOBE_XMP_MARKER.len()..body_end];
                let xmp = std::str::from_utf8(xmp_bytes)
                    .map_err(|e| XmpError::Xml(format!("XMP not UTF-8: {e}")))?;
                return Ok(Some(xmp.to_string()));
            }
            continue;
        }
        // Reached a non-app marker — XMP not present.
        return Ok(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_edit() -> EmbeddedEdit {
        EmbeddedEdit {
            original_asset_id: "e4f66d10-b88f-44e7-89ce-79259805a3b3".into(),
            original_content_hash: "uf1I8QpFndkfh4yAQn+SwZWvLZg=".into(),
            edit_version: 1,
            rendered_at: DateTime::parse_from_rfc3339("2026-05-10T12:34:56+00:00")
                .unwrap()
                .with_timezone(&Utc),
            edit_json: r#"{"version":1,"exposure":{"brightness":0.5}}"#.into(),
        }
    }

    #[test]
    fn encode_then_decode_round_trips() {
        let original = sample_edit();
        let xml = encode(&original);
        let decoded = decode(&xml).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn encode_escapes_xml_special_chars_in_edit_json() {
        // editJson is JSON which is largely XML-safe, but `<` could
        // appear in a string field. Verify it's escaped, and that the
        // decoded payload is bytewise-equal to the input.
        let original = EmbeddedEdit {
            edit_json: r#"{"label":"a < b & c > d"}"#.into(),
            ..sample_edit()
        };
        let xml = encode(&original);
        assert!(xml.contains("&lt;"));
        assert!(xml.contains("&gt;"));
        assert!(xml.contains("&amp;"));
        let decoded = decode(&xml).unwrap();
        assert_eq!(decoded.edit_json, original.edit_json);
    }

    #[test]
    fn decode_tolerates_attribute_reordering() {
        // Immich's media pipeline could rewrite RDF attributes. Our
        // decoder should match by local name + namespace, not exact
        // attribute order. Construct a "rewritten" form and verify.
        let xml = r#"<?xml version="1.0"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:moments="urn:moments:edits:1.0" rdf:about=""><moments:originalAssetId>asset-1</moments:originalAssetId><moments:originalContentHash>hash-1</moments:originalContentHash><moments:editVersion>1</moments:editVersion><moments:renderedAt>2026-05-10T12:34:56+00:00</moments:renderedAt><moments:editJson>{}</moments:editJson></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let edit = decode(xml).unwrap();
        assert_eq!(edit.original_asset_id, "asset-1");
        assert_eq!(edit.edit_version, 1);
    }

    #[test]
    fn decode_missing_field_returns_error() {
        let xml = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:moments="urn:moments:edits:1.0"><moments:originalAssetId>asset-1</moments:originalAssetId></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let err = decode(xml).unwrap_err();
        assert!(matches!(err, XmpError::MissingField(_)));
    }

    #[test]
    fn decode_invalid_version_returns_error() {
        let xml = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:moments="urn:moments:edits:1.0"><moments:originalAssetId>a</moments:originalAssetId><moments:originalContentHash>h</moments:originalContentHash><moments:editVersion>not-a-number</moments:editVersion><moments:renderedAt>2026-05-10T12:34:56+00:00</moments:renderedAt><moments:editJson>{}</moments:editJson></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let err = decode(xml).unwrap_err();
        assert!(
            matches!(
                err,
                XmpError::InvalidField {
                    field: "editVersion",
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    // ── JPEG segment walker tests ────────────────────────────────────

    /// Build a minimal JPEG: SOI + APP0 (JFIF) + DQT + EOI.
    /// Not decodable as an image, but valid for segment-walking tests.
    fn minimal_jpeg() -> Vec<u8> {
        let mut j = Vec::new();
        j.extend_from_slice(&[0xFF, 0xD8]); // SOI
                                            // APP0 JFIF: marker + length(16) + "JFIF\0" + 1.01 + ...
        j.extend_from_slice(&[0xFF, 0xE0]);
        let app0_payload = b"JFIF\x00\x01\x01\x00\x00\x01\x00\x01\x00\x00";
        j.extend_from_slice(&((app0_payload.len() + 2) as u16).to_be_bytes());
        j.extend_from_slice(app0_payload);
        // DQT (we won't supply real data, just a stub segment)
        j.extend_from_slice(&[0xFF, 0xDB]);
        let dqt_payload = [0u8; 64];
        j.extend_from_slice(&((dqt_payload.len() + 2) as u16).to_be_bytes());
        j.extend_from_slice(&dqt_payload);
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI
        j
    }

    #[test]
    fn jpeg_with_xmp_contains_marker_and_payload() {
        let jpeg = minimal_jpeg();
        let xmp = encode(&sample_edit());
        let out = inject_xmp(&jpeg, &xmp).unwrap();
        // APP0 should still be present and unchanged.
        assert_eq!(&out[2..4], &[0xFF, 0xE0]);
        // Marker bytes appear in the output.
        assert!(out
            .windows(ADOBE_XMP_MARKER.len())
            .any(|w| w == ADOBE_XMP_MARKER));
        // Original payload bytes appear too.
        assert!(out.windows(20).any(|w| w.starts_with(b"<x:xmpmeta")));
    }

    #[test]
    fn inject_then_extract_round_trips_xmp_text() {
        let jpeg = minimal_jpeg();
        let xmp = encode(&sample_edit());
        let out = inject_xmp(&jpeg, &xmp).unwrap();
        let extracted = extract_xmp(&out).unwrap().unwrap();
        assert_eq!(extracted, xmp);
    }

    #[test]
    fn inject_inserts_after_existing_app_segments() {
        // Verify the new APP1 lands AFTER APP0, not before.
        let jpeg = minimal_jpeg();
        let xmp = encode(&sample_edit());
        let out = inject_xmp(&jpeg, &xmp).unwrap();
        // Walk: SOI APP0 [our APP1] DQT EOI
        // SOI = bytes 0..2; first segment after SOI should be APP0.
        assert_eq!(&out[0..2], &[0xFF, 0xD8]);
        assert_eq!(&out[2..4], &[0xFF, 0xE0]);
        // skip APP0 length + payload
        let app0_len = u16::from_be_bytes([out[4], out[5]]) as usize;
        let after_app0 = 4 + app0_len;
        assert_eq!(&out[after_app0..after_app0 + 2], &[0xFF, 0xE1]);
    }

    #[test]
    fn extract_returns_none_when_no_xmp_segment() {
        let jpeg = minimal_jpeg();
        assert!(extract_xmp(&jpeg).unwrap().is_none());
    }

    #[test]
    fn extract_skips_non_xmp_app1_segments() {
        // Build a JPEG with an APP1 EXIF segment but no XMP — extract
        // should skip the EXIF block and not return its bytes.
        let mut jpeg = Vec::new();
        jpeg.extend_from_slice(&[0xFF, 0xD8]); // SOI
        jpeg.extend_from_slice(&[0xFF, 0xE1]); // APP1
        let exif_payload = b"Exif\x00\x00MM\x00\x2A\x00\x00\x00\x08";
        jpeg.extend_from_slice(&((exif_payload.len() + 2) as u16).to_be_bytes());
        jpeg.extend_from_slice(exif_payload);
        jpeg.extend_from_slice(&[0xFF, 0xD9]); // EOI

        assert!(extract_xmp(&jpeg).unwrap().is_none());
    }

    #[test]
    fn inject_rejects_truncated_jpeg() {
        // No SOI = invalid JPEG.
        let err = inject_xmp(b"\x00\x00", "x").unwrap_err();
        assert!(matches!(err, XmpError::Jpeg(_)));
    }

    #[test]
    fn inject_rejects_oversize_xmp() {
        let jpeg = minimal_jpeg();
        let oversize = "x".repeat(MAX_APP1_PAYLOAD + 1);
        let err = inject_xmp(&jpeg, &oversize).unwrap_err();
        assert!(matches!(err, XmpError::TooLarge { .. }));
    }

    #[test]
    fn round_trip_through_jpeg_preserves_embedded_edit() {
        // The full intended use: encode → inject → extract → decode →
        // recover the original struct.
        let original = sample_edit();
        let jpeg = minimal_jpeg();
        let with_xmp = inject_xmp(&jpeg, &encode(&original)).unwrap();
        let extracted_xml = extract_xmp(&with_xmp).unwrap().unwrap();
        let recovered = decode(&extracted_xml).unwrap();
        assert_eq!(recovered, original);
    }
}
