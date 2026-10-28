// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! GNOME Keyring integration for storing Immich session tokens.
//!
//! Thin wrapper around `libsecret` — the keyring is an application-level
//! concern (read at startup, written during setup) and does not depend on
//! the library layer.
//!
//! Every call uses libsecret's GIO async (`*_future`) API so a slow secret
//! service or an unlock prompt never blocks the GTK main thread (#704).
//! The futures are `!Send` and must be awaited on the GTK main context
//! (`glib::MainContext::spawn_local`); they need no Tokio runtime.
//!
//! Each call is also bounded by a 30 s timeout: if the secret service never
//! answers (or an unlock prompt is dismissed without an error) the call
//! returns `Err` instead of leaving the UI waiting forever.

use std::future::Future;
use std::time::Duration;

use futures_util::future::{select, Either};
use tracing::{debug, instrument, warn};

/// How long a keyring call may take before it is abandoned.
const KEYRING_TIMEOUT: Duration = Duration::from_secs(30);

/// Race `fut` against [`KEYRING_TIMEOUT`].
async fn with_timeout<T>(
    fut: impl Future<Output = Result<T, String>>,
    op: &'static str,
) -> Result<T, String> {
    with_timeout_for(fut, op, KEYRING_TIMEOUT).await
}

async fn with_timeout_for<T>(
    fut: impl Future<Output = Result<T, String>>,
    op: &'static str,
    timeout: Duration,
) -> Result<T, String> {
    let fut = std::pin::pin!(fut);
    let timer = std::pin::pin!(gtk::glib::timeout_future(timeout));
    match select(fut, timer).await {
        Either::Left((result, _)) => result,
        Either::Right(_) => {
            warn!(op, "keyring call timed out");
            Err(format!("{op} timed out after {timeout:?}"))
        }
    }
}

/// Store an Immich session token in the GNOME Keyring.
///
/// The token is associated with the server URL so multiple Immich servers
/// can each have their own stored credential.
#[instrument(skip(access_token), fields(server_url = %server_url))]
pub async fn store_access_token(server_url: &str, access_token: &str) -> Result<(), String> {
    let schema = schema();
    let attributes = std::collections::HashMap::from([("server_url", server_url)]);
    let label = format!("Moments — Immich session for {server_url}");

    with_timeout(
        async {
            libsecret::password_store_future(
                Some(&schema),
                attributes,
                Some(libsecret::COLLECTION_DEFAULT),
                &label,
                access_token,
            )
            .await
            .map_err(|e| format!("failed to store token in keyring: {e}"))
        },
        "storing token in keyring",
    )
    .await?;

    debug!("access token stored in keyring");
    Ok(())
}

/// Retrieve an Immich session token from the GNOME Keyring.
///
/// Returns `None` if no token is stored for this server URL.
#[instrument(fields(server_url = %server_url))]
pub async fn lookup_access_token(server_url: &str) -> Result<Option<String>, String> {
    let schema = schema();
    let attributes = std::collections::HashMap::from([("server_url", server_url)]);

    let secret = with_timeout(
        async {
            libsecret::password_lookup_future(Some(&schema), attributes)
                .await
                .map_err(|e| format!("failed to lookup token in keyring: {e}"))
        },
        "token lookup in keyring",
    )
    .await?;

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
pub async fn resolve_immich_token(server_url: &str) -> Result<String, TokenError> {
    map_lookup_result(lookup_access_token(server_url).await)
}

fn map_lookup_result(result: Result<Option<String>, String>) -> Result<String, TokenError> {
    match result {
        Ok(Some(token)) if !token.is_empty() => Ok(token),
        Ok(_) => Err(TokenError::Missing),
        Err(e) => Err(TokenError::KeyringFailed(e)),
    }
}

/// Delete a stored Immich session token from the GNOME Keyring.
///
/// A timeout error does not mean the token was left untouched or removed:
/// the dropped libsecret future may still complete in the background. Callers
/// must not read `Err` from a timeout as either "still stored" or "deleted";
/// re-check with [`lookup_access_token`] if the outcome matters.
#[allow(dead_code)] // Will be called by logout flow (not yet implemented)
#[instrument(fields(server_url = %server_url))]
pub async fn delete_access_token(server_url: &str) -> Result<(), String> {
    let schema = schema();
    let attributes = std::collections::HashMap::from([("server_url", server_url)]);

    with_timeout(
        async {
            libsecret::password_clear_future(Some(&schema), attributes)
                .await
                .map_err(|e| format!("failed to delete token from keyring: {e}"))
        },
        "deleting token from keyring",
    )
    .await?;

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
    fn with_timeout_returns_inner_result_when_first() {
        let ctx = gtk::glib::MainContext::new();
        let r = ctx.block_on(with_timeout_for(
            async { Ok::<_, String>(7) },
            "test op",
            Duration::from_secs(5),
        ));
        assert_eq!(r, Ok(7));
    }

    #[test]
    fn with_timeout_errors_on_never_resolving_future() {
        let ctx = gtk::glib::MainContext::new();
        let r: Result<(), String> = ctx.block_on(with_timeout_for(
            std::future::pending(),
            "test op",
            Duration::from_millis(10),
        ));
        assert_eq!(r, Err("test op timed out after 10ms".to_string()));
    }

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
