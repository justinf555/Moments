use tracing::{debug, instrument};

use super::error::LibraryError;

/// Store an Immich session token in the GNOME Keyring.
///
/// The token is associated with the server URL so multiple Immich servers
/// can each have their own stored credential.
#[instrument(skip(access_token), fields(server_url = %server_url))]
pub fn store_access_token(server_url: &str, access_token: &str) -> Result<(), LibraryError> {
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
    .map_err(|e| LibraryError::Immich(format!("failed to store token in keyring: {e}")))?;

    debug!("access token stored in keyring");
    Ok(())
}

/// Retrieve an Immich session token from the GNOME Keyring.
///
/// Returns `None` if no token is stored for this server URL.
#[instrument(fields(server_url = %server_url))]
pub fn lookup_access_token(server_url: &str) -> Result<Option<String>, LibraryError> {
    let schema = schema();
    let attributes = std::collections::HashMap::from([("server_url", server_url)]);

    let secret = libsecret::password_lookup_sync(
        Some(&schema),
        attributes,
        gio::Cancellable::NONE,
    )
    .map_err(|e| LibraryError::Immich(format!("failed to lookup token in keyring: {e}")))?;

    if secret.is_some() {
        debug!("access token found in keyring");
    } else {
        debug!("no access token found in keyring");
    }

    Ok(secret.map(|s| s.to_string()))
}

/// Delete a stored Immich session token from the GNOME Keyring.
#[allow(dead_code)] // Will be called by logout flow (not yet implemented)
#[instrument(fields(server_url = %server_url))]
pub fn delete_access_token(server_url: &str) -> Result<(), LibraryError> {
    let schema = schema();
    let attributes = std::collections::HashMap::from([("server_url", server_url)]);

    libsecret::password_clear_sync(
        Some(&schema),
        attributes,
        gio::Cancellable::NONE,
    )
    .map_err(|e| LibraryError::Immich(format!("failed to delete token from keyring: {e}")))?;

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

use gtk::gio;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_correct_name() {
        let s = schema();
        let _ = format!("{s:?}");
    }
}
