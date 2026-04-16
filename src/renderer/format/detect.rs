use std::io::Read;
use std::path::Path;

/// Detected image format from magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Jpeg,
    Png,
    WebP,
    Gif,
    Tiff,
    Heif,
}

/// Detected video format from magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFormat {
    Mp4,
    Mov,
    Mkv,
    Avi,
}

/// Result of content-based format detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedFormat {
    Image(ImageFormat),
    Video(VideoFormat),
    /// File does not match any known signature (e.g. RAW formats,
    /// corrupt files, or non-media files).
    Unknown,
}

/// Detect the media format of a file by reading its first 12 bytes.
///
/// Returns [`DetectedFormat::Unknown`] for unrecognised signatures,
/// empty files, or files shorter than 12 bytes. RAW camera formats
/// are intentionally not sniffed (too many proprietary variants with
/// overlapping TIFF-based headers) — the caller should fall back to
/// extension-based detection for those.
pub fn detect_format(path: &Path) -> std::io::Result<DetectedFormat> {
    let mut buf = [0u8; 12];
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DetectedFormat::Unknown);
        }
        Err(e) => return Err(e),
    };

    let n = file.read(&mut buf)?;
    if n < 4 {
        return Ok(DetectedFormat::Unknown);
    }

    Ok(detect_from_bytes(&buf[..n]))
}

/// Match a byte buffer against known magic signatures.
fn detect_from_bytes(buf: &[u8]) -> DetectedFormat {
    // JPEG: FF D8 FF
    if buf.len() >= 3 && buf[0] == 0xFF && buf[1] == 0xD8 && buf[2] == 0xFF {
        return DetectedFormat::Image(ImageFormat::Jpeg);
    }

    // PNG: 89 50 4E 47 0D 0A 1A 0A
    if buf.len() >= 8 && buf[..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A] {
        return DetectedFormat::Image(ImageFormat::Png);
    }

    // GIF: GIF87a or GIF89a
    if buf.len() >= 6 && (buf[..6] == *b"GIF87a" || buf[..6] == *b"GIF89a") {
        return DetectedFormat::Image(ImageFormat::Gif);
    }

    // TIFF: II (little-endian) or MM (big-endian)
    if buf.len() >= 4
        && ((buf[0] == 0x49 && buf[1] == 0x49 && buf[2] == 0x2A && buf[3] == 0x00)
            || (buf[0] == 0x4D && buf[1] == 0x4D && buf[2] == 0x00 && buf[3] == 0x2A))
    {
        return DetectedFormat::Image(ImageFormat::Tiff);
    }

    // MKV/WebM: EBML header 1A 45 DF A3
    if buf.len() >= 4 && buf[..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        return DetectedFormat::Video(VideoFormat::Mkv);
    }

    // RIFF container: WebP or AVI
    if buf.len() >= 12 && buf[..4] == *b"RIFF" {
        if buf[8..12] == *b"WEBP" {
            return DetectedFormat::Image(ImageFormat::WebP);
        }
        if buf[8..12] == *b"AVI " {
            return DetectedFormat::Video(VideoFormat::Avi);
        }
    }

    // ISO Base Media File Format (ftyp box): HEIF, MP4, MOV
    if buf.len() >= 12 && buf[4..8] == *b"ftyp" {
        let brand = &buf[8..12];
        return detect_ftyp_brand(brand);
    }

    DetectedFormat::Unknown
}

/// Distinguish HEIF, MP4, and MOV based on the ftyp major brand.
fn detect_ftyp_brand(brand: &[u8]) -> DetectedFormat {
    // HEIF/HEIC brands
    if brand == b"heic" || brand == b"heix" || brand == b"mif1" || brand == b"msf1" {
        return DetectedFormat::Image(ImageFormat::Heif);
    }

    // QuickTime
    if brand == b"qt  " {
        return DetectedFormat::Video(VideoFormat::Mov);
    }

    // MP4 / M4V / 3GP — all treated as MP4 for our purposes
    if brand == b"isom"
        || brand == b"iso2"
        || brand == b"mp41"
        || brand == b"mp42"
        || brand == b"M4V "
        || brand == b"M4A "
        || brand == b"3gp4"
        || brand == b"3gp5"
        || brand == b"3gp6"
        || brand == b"avc1"
    {
        return DetectedFormat::Video(VideoFormat::Mp4);
    }

    // Unknown ftyp brand — could be a newer HEIF or MP4 variant.
    // Conservative: return Unknown so extension fallback kicks in.
    DetectedFormat::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn detect_bytes(bytes: &[u8]) -> DetectedFormat {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        detect_format(f.path()).unwrap()
    }

    #[test]
    fn detect_jpeg() {
        assert_eq!(
            detect_bytes(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]),
            DetectedFormat::Image(ImageFormat::Jpeg),
        );
    }

    #[test]
    fn detect_png() {
        assert_eq!(
            detect_bytes(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D]),
            DetectedFormat::Image(ImageFormat::Png),
        );
    }

    #[test]
    fn detect_webp() {
        assert_eq!(
            detect_bytes(b"RIFF\x00\x00\x00\x00WEBP"),
            DetectedFormat::Image(ImageFormat::WebP),
        );
    }

    #[test]
    fn detect_gif() {
        assert_eq!(
            detect_bytes(b"GIF89a\x00\x00\x00\x00\x00\x00"),
            DetectedFormat::Image(ImageFormat::Gif),
        );
    }

    #[test]
    fn detect_tiff_little_endian() {
        assert_eq!(
            detect_bytes(&[0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00]),
            DetectedFormat::Image(ImageFormat::Tiff),
        );
    }

    #[test]
    fn detect_tiff_big_endian() {
        assert_eq!(
            detect_bytes(&[0x4D, 0x4D, 0x00, 0x2A, 0x00, 0x00, 0x00, 0x08]),
            DetectedFormat::Image(ImageFormat::Tiff),
        );
    }

    #[test]
    fn detect_heif() {
        // ftyp box with "heic" brand
        assert_eq!(
            detect_bytes(b"\x00\x00\x00\x18ftypheic"),
            DetectedFormat::Image(ImageFormat::Heif),
        );
    }

    #[test]
    fn detect_mp4() {
        assert_eq!(
            detect_bytes(b"\x00\x00\x00\x18ftypisom"),
            DetectedFormat::Video(VideoFormat::Mp4),
        );
    }

    #[test]
    fn detect_mov() {
        assert_eq!(
            detect_bytes(b"\x00\x00\x00\x14ftypqt  "),
            DetectedFormat::Video(VideoFormat::Mov),
        );
    }

    #[test]
    fn detect_mkv() {
        assert_eq!(
            detect_bytes(&[0x1A, 0x45, 0xDF, 0xA3, 0x93, 0x42, 0x82, 0x88]),
            DetectedFormat::Video(VideoFormat::Mkv),
        );
    }

    #[test]
    fn detect_avi() {
        assert_eq!(
            detect_bytes(b"RIFF\x00\x00\x00\x00AVI "),
            DetectedFormat::Video(VideoFormat::Avi),
        );
    }

    #[test]
    fn detect_unknown_bytes() {
        assert_eq!(
            detect_bytes(&[0x00, 0x01, 0x02, 0x03, 0x04, 0x05]),
            DetectedFormat::Unknown,
        );
    }

    #[test]
    fn detect_empty_file() {
        let f = NamedTempFile::new().unwrap();
        assert_eq!(detect_format(f.path()).unwrap(), DetectedFormat::Unknown);
    }

    #[test]
    fn detect_short_file() {
        assert_eq!(detect_bytes(&[0xFF, 0xD8]), DetectedFormat::Unknown);
    }
}
