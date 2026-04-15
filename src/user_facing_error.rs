//! User-facing error trait.
//!
//! Separates human-readable error messages (for toasts, dialogs) from
//! technical details (for logs). Each error type implements this trait
//! to provide a translated, non-technical message.
//!
//! Implementations should use `gettextrs::gettext()` for translation.
//!
//! Inspired by Fractal's `UserFacingError` pattern.

/// Convert an error into a human-readable message for UI display.
///
/// Separate from [`std::fmt::Display`] (which is for logs) and
/// [`std::fmt::Debug`] (which is for developers). The message
/// returned here is shown to the user in toasts and dialogs.
pub trait UserFacingError {
    fn to_user_facing(&self) -> String;
}
