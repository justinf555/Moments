use super::error::LibraryError;

/// Events emitted by the library backend and delivered to the GTK application.
///
/// The GTK layer creates a `std::sync::mpsc::channel::<LibraryEvent>()`, passes
/// the `Sender` into `LibraryFactory::create`, and polls the `Receiver` via
/// `glib::idle_add`. The library never imports GTK types.
///
/// Additional variants will be added as features are implemented.
#[derive(Debug)]
pub enum LibraryEvent {
    /// The library has finished opening and is ready to accept operations.
    Ready,

    /// The library has fully shut down after a `close()` call.
    ShutdownComplete,

    /// A non-fatal error occurred in a background operation.
    Error(LibraryError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_event_is_debug() {
        let event = LibraryEvent::Ready;
        assert!(format!("{event:?}").contains("Ready"));
    }

    #[test]
    fn shutdown_complete_is_debug() {
        let event = LibraryEvent::ShutdownComplete;
        assert!(format!("{event:?}").contains("ShutdownComplete"));
    }

    #[test]
    fn error_event_wraps_library_error() {
        let event = LibraryEvent::Error(LibraryError::Bundle("test".to_string()));
        assert!(format!("{event:?}").contains("Error"));
    }
}
