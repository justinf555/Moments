/// A unique identifier for an asset (photo or video) within the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssetId(u64);

impl AssetId {
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for AssetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A unique identifier for a detected face within the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FaceId(u64);

impl FaceId {
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for FaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_id_roundtrip() {
        let id = AssetId::new(42);
        assert_eq!(id.value(), 42);
    }

    #[test]
    fn asset_id_display() {
        assert_eq!(AssetId::new(42).to_string(), "42");
    }

    #[test]
    fn asset_id_equality() {
        assert_eq!(AssetId::new(1), AssetId::new(1));
        assert_ne!(AssetId::new(1), AssetId::new(2));
    }

    #[test]
    fn asset_id_is_copy() {
        let id = AssetId::new(1);
        let _copy = id;
        let _ = id; // still usable after copy
    }

    #[test]
    fn face_id_roundtrip() {
        let id = FaceId::new(7);
        assert_eq!(id.value(), 7);
    }

    #[test]
    fn face_id_display() {
        assert_eq!(FaceId::new(7).to_string(), "7");
    }

    #[test]
    fn face_id_equality() {
        assert_eq!(FaceId::new(3), FaceId::new(3));
        assert_ne!(FaceId::new(3), FaceId::new(4));
    }

    #[test]
    fn face_id_is_copy() {
        let id = FaceId::new(1);
        let _copy = id;
        let _ = id; // still usable after copy
    }
}
