use super::super::media::MediaId;

/// A row in the `media_metadata` table — full EXIF detail, loaded on demand.
#[derive(Debug, Clone)]
pub struct MediaMetadataRecord {
    pub media_id: MediaId,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens_model: Option<String>,
    pub aperture: Option<f32>,
    pub shutter_str: Option<String>,
    pub iso: Option<u32>,
    pub focal_length: Option<f32>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub gps_alt: Option<f64>,
    pub color_space: Option<String>,
}

impl MediaMetadataRecord {
    /// Returns `true` if at least one field is populated.
    ///
    /// Used to skip inserting an empty row for assets with no EXIF metadata.
    pub fn has_data(&self) -> bool {
        self.camera_make.is_some()
            || self.camera_model.is_some()
            || self.lens_model.is_some()
            || self.aperture.is_some()
            || self.shutter_str.is_some()
            || self.iso.is_some()
            || self.focal_length.is_some()
            || self.gps_lat.is_some()
            || self.gps_lon.is_some()
            || self.gps_alt.is_some()
            || self.color_space.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_data_false_when_all_none() {
        let record = MediaMetadataRecord {
            media_id: MediaId::new("a".repeat(64)),
            camera_make: None,
            camera_model: None,
            lens_model: None,
            aperture: None,
            shutter_str: None,
            iso: None,
            focal_length: None,
            gps_lat: None,
            gps_lon: None,
            gps_alt: None,
            color_space: None,
        };
        assert!(!record.has_data());
    }

    #[test]
    fn has_data_true_with_single_field() {
        let record = MediaMetadataRecord {
            media_id: MediaId::new("b".repeat(64)),
            camera_make: Some("Canon".to_string()),
            camera_model: None,
            lens_model: None,
            aperture: None,
            shutter_str: None,
            iso: None,
            focal_length: None,
            gps_lat: None,
            gps_lon: None,
            gps_alt: None,
            color_space: None,
        };
        assert!(record.has_data());
    }
}
