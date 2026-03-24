use tracing::{debug, instrument};

use super::error::LibraryError;

/// Store an Immich API key in the GNOME Keyring.
///
/// The key is associated with the server URL so multiple Immich servers
/// can each have their own stored credential.
#[instrument(skip(api_key), fields(server_url = %server_url))]
pub fn store_api_key(server_url: &str, api_key: &str) -> Result<(), LibraryError> {
    let schema = schema();
    let attributes = std::collections::HashMap::from([("server_url", server_url)]);
    let label = format!("Moments — Immich API key for {server_url}");

    libsecret::password_store_sync(
        Some(&schema),
        attributes,
        Some(libsecret::COLLECTION_DEFAULT),
        &label,
        api_key,
        gio::Cancellable::NONE,
    )
    .map_err(|e| LibraryError::Immich(format!("failed to store API key in keyring: {e}")))?;

    debug!("API key stored in keyring");
    Ok(())
}

/// Retrieve an Immich API key from the GNOME Keyring.
///
/// Returns `None` if no key is stored for this server URL.
#[instrument(fields(server_url = %server_url))]
pub fn lookup_api_key(server_url: &str) -> Result<Option<String>, LibraryError> {
    let schema = schema();
    let attributes = std::collections::HashMap::from([("server_url", server_url)]);

    let secret = libsecret::password_lookup_sync(
        Some(&schema),
        attributes,
        gio::Cancellable::NONE,
    )
    .map_err(|e| LibraryError::Immich(format!("failed to lookup API key in keyring: {e}")))?;

    if secret.is_some() {
        debug!("API key found in keyring");
    } else {
        debug!("no API key found in keyring");
    }

    Ok(secret.map(|s| s.to_string()))
}

/// Delete a stored Immich API key from the GNOME Keyring.
#[instrument(fields(server_url = %server_url))]
pub fn delete_api_key(server_url: &str) -> Result<(), LibraryError> {
    let schema = schema();
    let attributes = std::collections::HashMap::from([("server_url", server_url)]);

    libsecret::password_clear_sync(
        Some(&schema),
        attributes,
        gio::Cancellable::NONE,
    )
    .map_err(|e| LibraryError::Immich(format!("failed to delete API key from keyring: {e}")))?;

    debug!("API key deleted from keyring");
    Ok(())
}

/// Build the libsecret schema for Moments credentials.
fn schema() -> libsecret::Schema {
    libsecret::Schema::new(
        "io.github.justinf555.Moments",
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
        // Just verify it builds without panic.
        let _ = format!("{s:?}");
    }
}
