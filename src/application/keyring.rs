//! GNOME Keyring integration for storing Immich session tokens.
//!
//! Thin wrapper around `libsecret` — the keyring is an application-level
//! concern (read at startup, written during setup) and does not depend on
//! the library layer.

use gtk::gio;
use tracing::{debug, instrument};

/// Store an Immich session token in the GNOME Keyring.
///
/// The token is associated with the server URL so multiple Immich servers
/// can each have their own stored credential.
#[instrument(skip(access_token), fields(server_url = %server_url))]
pub fn store_access_token(server_url: &str, access_token: &str) -> Result<(), String> {
    let schema = schema();
    let attributes = std::collections::HashMap::from([("server_url", server_url)]);
    let label = format!("Moments — Immich session for {server_url}");

    libsecret::password_store_sync(
        Some(&schema),
        attributes,
        Some(libsecret::COLLECTION_DEFAULT),
        &label,
        access_token,
        gio::Cancellable::NONE,
    )
    .map_err(|e| format!("failed to store token in keyring: {e}"))?;

    debug!("access token stored in keyring");
    Ok(())
}

/// Retrieve an Immich session token from the GNOME Keyring.
///
/// Returns `None` if no token is stored for this server URL.
#[instrument(fields(server_url = %server_url))]
pub fn lookup_access_token(server_url: &str) -> Result<Option<String>, String> {
    let schema = schema();
    let attributes = std::collections::HashMap::from([("server_url", server_url)]);

    let secret = libsecret::password_lookup_sync(Some(&schema), attributes, gio::Cancellable::NONE)
        .map_err(|e| format!("failed to lookup token in keyring: {e}"))?;

    if secret.is_some() {
        debug!("access token found in keyring");
    } else {
        debug!("no access token found in keyring");
    }

    Ok(secret.map(|s| s.to_string()))
}

/// Why an Immich session token could not be resolved at startup.
///
/// Distinguishes the three states callers need to surface differently:
/// a legitimately missing entry (user must sign in again), an empty
/// stored value (treated as missing), and a keyring/D-Bus failure
/// (system-level problem the user should see).
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    /// No keyring entry exists for this server, or the stored value
    /// was empty.
    #[error("no keyring entry for the configured Immich server")]
    Missing,
    /// libsecret returned an error (e.g. D-Bus unavailable, locked
    /// collection, schema mismatch). The string is the underlying error.
    #[error("keyring lookup failed: {0}")]
    KeyringFailed(String),
}

/// Resolve an Immich session token from the keyring, collapsing the
/// three keyring outcomes into a strict `Ok(non-empty token)` /
/// `Err(reason)` shape so call sites cannot accidentally pass an
/// empty string downstream.
pub fn resolve_immich_token(server_url: &str) -> Result<String, TokenError> {
    map_lookup_result(lookup_access_token(server_url))
}

fn map_lookup_result(result: Result<Option<String>, String>) -> Result<String, TokenError> {
    match result {
        Ok(Some(token)) if !token.is_empty() => Ok(token),
        Ok(_) => Err(TokenError::Missing),
        Err(e) => Err(TokenError::KeyringFailed(e)),
    }
}

/// Delete a stored Immich session token from the GNOME Keyring.
#[allow(dead_code)] // Will be called by logout flow (not yet implemented)
#[instrument(fields(server_url = %server_url))]
pub fn delete_access_token(server_url: &str) -> Result<(), String> {
    let schema = schema();
    let attributes = std::collections::HashMap::from([("server_url", server_url)]);

    libsecret::password_clear_sync(Some(&schema), attributes, gio::Cancellable::NONE)
        .map_err(|e| format!("failed to delete token from keyring: {e}"))?;

    debug!("access token deleted from keyring");
    Ok(())
}

/// Build the libsecret schema for Moments credentials.
fn schema() -> libsecret::Schema {
    libsecret::Schema::new(
        crate::config::APP_ID,
        libsecret::SchemaFlags::NONE,
        std::collections::HashMap::from([("server_url", libsecret::SchemaAttributeType::String)]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_constructs_without_panic() {
        let s = schema();
        let _ = format!("{s:?}");
    }

    #[test]
    fn map_lookup_present_returns_ok() {
        let r = map_lookup_result(Ok(Some("abc".into())));
        assert!(matches!(r, Ok(t) if t == "abc"));
    }

    #[test]
    fn map_lookup_empty_string_treated_as_missing() {
        let r = map_lookup_result(Ok(Some(String::new())));
        assert!(matches!(r, Err(TokenError::Missing)));
    }

    #[test]
    fn map_lookup_none_is_missing() {
        let r = map_lookup_result(Ok(None));
        assert!(matches!(r, Err(TokenError::Missing)));
    }

    #[test]
    fn map_lookup_error_is_keyring_failed() {
        let r = map_lookup_result(Err("dbus down".into()));
        assert!(matches!(r, Err(TokenError::KeyringFailed(e)) if e == "dbus down"));
    }
}
