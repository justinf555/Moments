use super::error::LibraryError;
use super::types::{AssetId, FaceId};

/// Events emitted by the library backend and delivered to the GTK application.
///
/// The GTK layer creates a `std::sync::mpsc::channel::<LibraryEvent>()`, passes
/// the `Sender` into `LibraryFactory::create`, and polls the `Receiver` via
/// `glib::idle_add`. The library never imports GTK types.
#[derive(Debug)]
pub enum LibraryEvent {
    /// The library has finished opening and is ready to accept operations.
    Ready,

    /// An asset has been successfully imported into the library.
    AssetImported { asset_id: AssetId },

    /// A thumbnail has been generated for an asset.
    ThumbnailCreated { asset_id: AssetId },

    /// EXIF and file metadata has been extracted for an asset.
    MetadataExtracted { asset_id: AssetId },

    /// A face has been detected within an asset.
    FaceDetected { asset_id: AssetId, face_id: FaceId },

    /// Progress update during a batch import operation.
    ImportProgress { current: u32, total: u32 },

    /// A batch import operation has completed.
    ImportComplete,

    /// The library has fully shut down after a `close()` call.
    ShutdownComplete,

    /// A non-fatal error occurred in a background operation.
    Error(LibraryError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::{AssetId, FaceId};

    #[test]
    fn ready_event_is_debug() {
        let event = LibraryEvent::Ready;
        assert!(format!("{event:?}").contains("Ready"));
    }

    #[test]
    fn asset_imported_contains_id() {
        let event = LibraryEvent::AssetImported { asset_id: AssetId::new(10) };
        assert!(format!("{event:?}").contains("AssetImported"));
    }

    #[test]
    fn thumbnail_created_contains_id() {
        let event = LibraryEvent::ThumbnailCreated { asset_id: AssetId::new(5) };
        assert!(format!("{event:?}").contains("ThumbnailCreated"));
    }

    #[test]
    fn metadata_extracted_contains_id() {
        let event = LibraryEvent::MetadataExtracted { asset_id: AssetId::new(3) };
        assert!(format!("{event:?}").contains("MetadataExtracted"));
    }

    #[test]
    fn face_detected_contains_both_ids() {
        let event = LibraryEvent::FaceDetected {
            asset_id: AssetId::new(1),
            face_id: FaceId::new(2),
        };
        assert!(format!("{event:?}").contains("FaceDetected"));
    }

    #[test]
    fn import_progress_values() {
        let event = LibraryEvent::ImportProgress { current: 3, total: 10 };
        if let LibraryEvent::ImportProgress { current, total } = event {
            assert_eq!(current, 3);
            assert_eq!(total, 10);
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn error_event_wraps_library_error() {
        let event = LibraryEvent::Error(LibraryError::Bundle("test".to_string()));
        assert!(format!("{event:?}").contains("Error"));
    }

    #[test]
    fn shutdown_complete_is_debug() {
        let event = LibraryEvent::ShutdownComplete;
        assert!(format!("{event:?}").contains("ShutdownComplete"));
    }
}
