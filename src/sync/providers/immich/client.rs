//! HTTP client for the Immich server API.
//!
//! Uses session-based authentication (`Authorization: Bearer {token}`).
//! The session token is obtained via [`ImmichClient::login`] and stored
//! in the GNOME Keyring.

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use crate::library::error::LibraryError;

/// Truncate an API error body to avoid leaking server internals in toasts.
fn truncate_error_body(body: &str, max_len: usize) -> &str {
    match body.char_indices().nth(max_len) {
        Some((idx, _)) => &body[..idx],
        None => body,
    }
}

/// HTTP client for the Immich server API.
///
/// Uses session-based authentication (`Authorization: Bearer {token}`).
/// All methods are async and intended to run on the Tokio executor.
#[derive(Clone)]
pub struct ImmichClient {
    client: reqwest::Client,
    base_url: String,
}

impl ImmichClient {
    /// Create a new client with an existing session token.
    ///
    /// The `server_url` should be the root URL (e.g. `https://immich.example.com`).
    /// A trailing `/api` is appended automatically for endpoint calls.
    pub fn new(server_url: &str, access_token: &str) -> Result<Self, LibraryError> {
        let mut headers = HeaderMap::new();
        let auth_value = format!("Bearer {access_token}");
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth_value)
                .map_err(|e| LibraryError::Immich(format!("invalid access token: {e}")))?,
        );
        headers.insert("Accept", HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .user_agent("Moments/0.1")
            .build()
            .map_err(|e| LibraryError::Immich(format!("failed to build HTTP client: {e}")))?;

        let base_url = server_url.trim_end_matches('/').to_owned();

        Ok(Self { client, base_url })
    }

    /// Login to the Immich server with email and password.
    ///
    /// Returns a [`LoginResponse`] containing the session token and user info.
    /// The token should be stored in the GNOME Keyring and passed to [`new`](Self::new)
    /// for subsequent client construction.
    #[instrument(skip(password), fields(server_url = %server_url, email = %email))]
    pub async fn login(
        server_url: &str,
        email: &str,
        password: &str,
    ) -> Result<LoginResponse, LibraryError> {
        let base_url = server_url.trim_end_matches('/');
        let url = format!("{base_url}/api/auth/login");

        debug!("logging in to Immich server");

        let body = LoginRequest {
            email: email.to_owned(),
            password: password.to_owned(),
        };

        let client = reqwest::Client::builder()
            .user_agent("Moments/0.1")
            .build()
            .map_err(|e| LibraryError::Immich(format!("failed to build HTTP client: {e}")))?;

        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| LibraryError::Immich(format!("login failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LibraryError::Immich(format!(
                "login failed with status {status}: {}",
                truncate_error_body(&body, 200),
            )));
        }

        let login: LoginResponse = resp
            .json()
            .await
            .map_err(|e| LibraryError::Immich(format!("invalid login response: {e}")))?;

        debug!(user = %login.name, "login successful");
        Ok(login)
    }

    /// The base server URL (without trailing slash).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Build a full URL for an API endpoint path.
    fn url(&self, path: &str) -> String {
        format!("{}/api{}", self.base_url, path)
    }

    /// Ping the server to check connectivity.
    #[instrument(skip(self), fields(url = %self.base_url))]
    pub async fn ping(&self) -> Result<(), LibraryError> {
        let url = self.url("/server/ping");
        debug!("pinging server");

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| LibraryError::Immich(format!("connection failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(LibraryError::Immich(format!(
                "ping failed with status {status}"
            )));
        }

        let body: PingResponse = resp
            .json()
            .await
            .map_err(|e| LibraryError::Immich(format!("invalid ping response: {e}")))?;

        if body.res != "pong" {
            return Err(LibraryError::Immich(format!(
                "unexpected ping response: {}",
                body.res
            )));
        }

        debug!("server ping successful");
        Ok(())
    }

    /// Retrieve server version and build information.
    #[instrument(skip(self), fields(url = %self.base_url))]
    pub async fn server_about(&self) -> Result<ServerAbout, LibraryError> {
        let url = self.url("/server/about");
        debug!("fetching server info");

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| LibraryError::Immich(format!("connection failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(LibraryError::Immich(format!(
                "server about failed with status {status}"
            )));
        }

        let about: ServerAbout = resp
            .json()
            .await
            .map_err(|e| LibraryError::Immich(format!("invalid server about response: {e}")))?;

        debug!(version = %about.version, "server info retrieved");
        Ok(about)
    }

    /// Validate the connection by pinging and fetching server info.
    #[allow(dead_code)]
    #[instrument(skip(self), fields(url = %self.base_url))]
    pub async fn validate(&self) -> Result<ServerAbout, LibraryError> {
        self.ping().await?;
        self.server_about().await
    }

    // ── Private helpers ────────────────────────────────────────────────────

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        method: &str,
        path: &str,
    ) -> Result<reqwest::Response, LibraryError> {
        let resp = request.send().await.map_err(|e| {
            if e.is_connect() || e.is_timeout() {
                LibraryError::Connectivity(format!("{method} {path} failed: {e}"))
            } else {
                LibraryError::Immich(format!("{method} {path} failed: {e}"))
            }
        })?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LibraryError::Immich(format!(
                "{method} {path} returned {status}: {}",
                truncate_error_body(&body, 200),
            )));
        }

        Ok(resp)
    }

    async fn send_json<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
        method: &str,
        path: &str,
    ) -> Result<T, LibraryError> {
        let resp = self.send(request, method, path).await?;
        resp.json()
            .await
            .map_err(|e| LibraryError::Immich(format!("{method} {path} parse failed: {e}")))
    }

    async fn send_no_content(
        &self,
        request: reqwest::RequestBuilder,
        method: &str,
        path: &str,
    ) -> Result<(), LibraryError> {
        self.send(request, method, path).await?;
        Ok(())
    }

    // ── Typed HTTP methods ───────────────────────────────────────────────

    #[allow(dead_code)]
    pub(crate) async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<T, LibraryError> {
        self.send_json(self.client.get(self.url(path)), "GET", path)
            .await
    }

    pub(crate) async fn post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, LibraryError> {
        self.send_json(self.client.post(self.url(path)).json(body), "POST", path)
            .await
    }

    pub(crate) async fn post_no_content<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), LibraryError> {
        self.send_no_content(self.client.post(self.url(path)).json(body), "POST", path)
            .await
    }

    pub(crate) async fn put_no_content<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), LibraryError> {
        self.send_no_content(self.client.put(self.url(path)).json(body), "PUT", path)
            .await
    }

    pub(crate) async fn delete_no_content(&self, path: &str) -> Result<(), LibraryError> {
        self.send_no_content(self.client.delete(self.url(path)), "DELETE", path)
            .await
    }

    pub(crate) async fn delete_with_body<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), LibraryError> {
        self.send_no_content(
            self.client.delete(self.url(path)).json(body),
            "DELETE",
            path,
        )
        .await
    }

    pub(crate) async fn patch_no_content<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), LibraryError> {
        self.send_no_content(self.client.patch(self.url(path)).json(body), "PATCH", path)
            .await
    }

    /// Upload an asset via multipart form-data.
    ///
    /// `filename` must be the original user-facing filename (e.g.
    /// `IMG_1234.jpg`). It is sent in the multipart `Content-Disposition`
    /// header so Immich can infer the asset's media type from the
    /// extension. The on-disk path may be extensionless (UUID-sharded
    /// originals layout) so we cannot derive it from `file_path`.
    pub(crate) async fn upload_asset(
        &self,
        file_path: &std::path::Path,
        filename: &str,
        device_asset_id: &str,
        file_created_at: &str,
        file_modified_at: &str,
        checksum: Option<&str>,
    ) -> Result<UploadResponse, LibraryError> {
        let url = self.url("/assets");

        let file_bytes = tokio::fs::read(file_path).await.map_err(LibraryError::Io)?;

        let file_part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(filename.to_owned())
            .mime_str("application/octet-stream")
            .map_err(|e| LibraryError::Immich(format!("invalid mime type: {e}")))?;

        let form = reqwest::multipart::Form::new()
            .part("assetData", file_part)
            .text("deviceAssetId", device_asset_id.to_owned())
            .text("deviceId", "moments".to_owned())
            .text("fileCreatedAt", file_created_at.to_owned())
            .text("fileModifiedAt", file_modified_at.to_owned());

        let mut request = self.client.post(&url).multipart(form);

        if let Some(hash) = checksum {
            request = request.header("x-immich-checksum", hash);
        }

        let resp = request
            .send()
            .await
            .map_err(|e| LibraryError::Immich(format!("upload failed: {e}")))?;

        let status_code = resp.status();
        if !status_code.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LibraryError::Immich(format!(
                "upload returned {status_code}: {}",
                truncate_error_body(&body, 200),
            )));
        }

        let upload_status = if status_code.as_u16() == 200 {
            "duplicate".to_string()
        } else {
            "created".to_string()
        };

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| LibraryError::Immich(format!("invalid upload response: {e}")))?;

        let id = body["id"].as_str().unwrap_or_default().to_owned();

        Ok(UploadResponse {
            id,
            status: upload_status,
        })
    }

    /// Make a GET request and return the raw response bytes.
    pub(crate) async fn get_bytes(&self, path: &str) -> Result<Vec<u8>, LibraryError> {
        let resp = self
            .send(self.client.get(self.url(path)), "GET", path)
            .await?;
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| LibraryError::Immich(format!("GET {path} read failed: {e}")))
    }

    /// Replace the geometric edit list on an asset.
    ///
    /// PUT `/assets/{id}/edits` with `{ "edits": [...] }`. The server
    /// returns 200 with the stamped edit IDs in the response body, but
    /// we don't need them — the next pull cycle re-emits as
    /// `SyncAssetEditV1` records.
    pub(crate) async fn put_asset_edits<A: serde::Serialize>(
        &self,
        external_id: &str,
        actions: &[A],
    ) -> Result<(), LibraryError> {
        self.put_no_content(
            &format!("/assets/{external_id}/edits"),
            &serde_json::json!({ "edits": actions }),
        )
        .await
    }

    /// Clear the geometric edit list on an asset (revert).
    ///
    /// Verified idempotent on Immich v2.7.5 — DELETE on an asset with no
    /// edits returns 204, so we don't need a 404-tolerance guard here.
    /// Worth re-checking if a future Immich version changes this.
    pub(crate) async fn delete_asset_edits(&self, external_id: &str) -> Result<(), LibraryError> {
        self.delete_no_content(&format!("/assets/{external_id}/edits"))
            .await
    }

    // ── Stacks (Phase C, #224) ───────────────────────────────────────

    /// Create a stack containing the given assets. The first id in
    /// the slice becomes the server-side primary; Phase C uploads the
    /// rendered child first so it's the primary, and the §8.2 grid
    /// filter swaps the original back in for display.
    ///
    /// Returns the new stack's server id.
    pub(crate) async fn post_stack(
        &self,
        asset_ids: &[&str],
    ) -> Result<StackResponse, LibraryError> {
        self.post("/stacks", &serde_json::json!({ "assetIds": asset_ids }))
            .await
    }

    /// Remove one asset from a stack. On Immich v2.7.5 the stack
    /// record persists with a single member while the surviving
    /// asset's `stackId` is cleared on its row — local cleanup
    /// completes via the next pull's heartbeat reconciliation. Verified
    /// idempotent: 204 even if the asset is no longer in the stack.
    pub(crate) async fn delete_stack_member(
        &self,
        stack_id: &str,
        asset_id: &str,
    ) -> Result<(), LibraryError> {
        self.delete_no_content(&format!("/stacks/{stack_id}/assets/{asset_id}"))
            .await
    }

    // ── Tags (Phase C, #224) ─────────────────────────────────────────

    /// Idempotently ensure that the tags with the given names exist
    /// on the server, returning their resolved ids in input order.
    ///
    /// Uses `PUT /tags { tags: [...] }` — verified idempotent on
    /// v2.7.5 (returns existing rows untouched, creates missing ones).
    /// `POST /tags { name }` was rejected because it 400s on existing
    /// tags.
    pub(crate) async fn ensure_tags(
        &self,
        names: &[&str],
    ) -> Result<Vec<TagResponse>, LibraryError> {
        self.put_json("/tags", &serde_json::json!({ "tags": names }))
            .await
    }

    /// Attach `tag_id` to each of the given asset ids. The response
    /// is per-id; duplicates (already tagged) come back as
    /// `success: false, error: "duplicate"` and are not an error.
    pub(crate) async fn add_assets_to_tag(
        &self,
        tag_id: &str,
        asset_ids: &[&str],
    ) -> Result<(), LibraryError> {
        self.put_no_content(
            &format!("/tags/{tag_id}/assets"),
            &serde_json::json!({ "ids": asset_ids }),
        )
        .await
    }

    /// Detach `tag_id` from each of the given asset ids.
    pub(crate) async fn remove_assets_from_tag(
        &self,
        tag_id: &str,
        asset_ids: &[&str],
    ) -> Result<(), LibraryError> {
        self.delete_with_body(
            &format!("/tags/{tag_id}/assets"),
            &serde_json::json!({ "ids": asset_ids }),
        )
        .await
    }

    async fn put_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, LibraryError> {
        self.send_json(self.client.put(self.url(path)).json(body), "PUT", path)
            .await
    }

    /// Send a POST request and return the raw response for streaming.
    pub(crate) async fn post_stream<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<reqwest::Response, LibraryError> {
        self.send(self.client.post(self.url(path)).json(body), "POST", path)
            .await
    }
}

// ── Request/response types ──────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct LoginRequest {
    email: String,
    password: String,
}

/// Response from `POST /auth/login`.
#[derive(Debug, Clone, Deserialize)]
pub struct LoginResponse {
    #[serde(rename = "accessToken")]
    pub access_token: String,
    #[serde(rename = "userId")]
    pub user_id: String,
    pub name: String,
}

/// Response from `POST /assets` (upload).
#[derive(Debug, Clone)]
pub struct UploadResponse {
    pub id: String,
    /// "created" or "duplicate".
    pub status: String,
}

/// Response from `POST /stacks` — only the `id` and `primaryAssetId`
/// fields are needed. Immich's full response includes the assets array
/// but we get that via the next sync stream.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct StackResponse {
    pub id: String,
    #[serde(rename = "primaryAssetId")]
    pub primary_asset_id: String,
}

/// One element of the response from `PUT /tags { tags: [...] }`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct TagResponse {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
struct PingResponse {
    res: String,
}

/// Server version info from `GET /server/about`.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerAbout {
    pub version: String,
    #[serde(default)]
    pub licensed: bool,
}

impl std::fmt::Display for ServerAbout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Immich {}", self.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_normalises_trailing_slash() {
        let client = ImmichClient::new("https://immich.example.com/", "test-token").unwrap();
        assert_eq!(client.base_url(), "https://immich.example.com");
    }

    #[test]
    fn client_preserves_url_without_trailing_slash() {
        let client = ImmichClient::new("https://immich.example.com", "test-token").unwrap();
        assert_eq!(client.base_url(), "https://immich.example.com");
    }

    #[test]
    fn url_builds_api_path() {
        let client = ImmichClient::new("https://immich.example.com", "test-token").unwrap();
        assert_eq!(
            client.url("/server/ping"),
            "https://immich.example.com/api/server/ping"
        );
    }

    #[test]
    fn server_about_display() {
        let about = ServerAbout {
            version: "1.99.0".to_string(),
            licensed: false,
        };
        assert_eq!(format!("{about}"), "Immich 1.99.0");
    }

    #[test]
    fn client_strips_multiple_trailing_slashes() {
        let client = ImmichClient::new("https://immich.example.com///", "token").unwrap();
        // Only one trailing slash is stripped by trim_end_matches
        // so the URL may have double slashes — this tests the actual behavior.
        assert!(!client.base_url().ends_with('/'));
    }

    #[test]
    fn url_builds_correct_nested_path() {
        let client = ImmichClient::new("https://immich.example.com", "token").unwrap();
        assert_eq!(
            client.url("/assets/uuid-123/original"),
            "https://immich.example.com/api/assets/uuid-123/original"
        );
    }

    #[test]
    fn login_response_deserializes() {
        let json = serde_json::json!({
            "accessToken": "token-abc",
            "userId": "user-uuid",
            "name": "Test User"
        });
        let resp: LoginResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.access_token, "token-abc");
        assert_eq!(resp.user_id, "user-uuid");
        assert_eq!(resp.name, "Test User");
    }

    #[test]
    fn server_about_deserializes_with_defaults() {
        let json = serde_json::json!({ "version": "1.120.0" });
        let about: ServerAbout = serde_json::from_value(json).unwrap();
        assert_eq!(about.version, "1.120.0");
        assert!(!about.licensed); // default
    }

    #[test]
    fn server_about_deserializes_licensed() {
        let json = serde_json::json!({ "version": "1.120.0", "licensed": true });
        let about: ServerAbout = serde_json::from_value(json).unwrap();
        assert!(about.licensed);
    }

    #[test]
    fn truncate_error_body_short() {
        assert_eq!(truncate_error_body("hello", 10), "hello");
    }

    #[test]
    fn truncate_error_body_long() {
        let long = "a".repeat(300);
        let truncated = truncate_error_body(&long, 200);
        assert_eq!(truncated.len(), 200);
    }

    #[test]
    fn truncate_error_body_unicode() {
        // Emoji is multi-byte — should not panic or split a char.
        let emoji_str = "🎉".repeat(50);
        let truncated = truncate_error_body(&emoji_str, 10);
        // 10 emoji characters
        assert_eq!(truncated.chars().count(), 10);
    }

    #[test]
    fn stack_response_deserialises() {
        // Shape captured from the live v2.7.5 probe — we only consume
        // `id` and `primaryAssetId`; the `assets` array is ignored.
        let json = serde_json::json!({
            "id": "stk-uuid",
            "primaryAssetId": "rendered-uuid",
            "assets": [{"id":"rendered-uuid"},{"id":"orig-uuid"}]
        });
        let resp: StackResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.id, "stk-uuid");
        assert_eq!(resp.primary_asset_id, "rendered-uuid");
    }

    #[test]
    fn tag_response_deserialises() {
        let json = serde_json::json!([
            {"id":"tag-1","name":"moments-edit","value":"moments-edit","createdAt":"…","updatedAt":"…"},
            {"id":"tag-2","name":"other","value":"other","createdAt":"…","updatedAt":"…"}
        ]);
        let resp: Vec<TagResponse> = serde_json::from_value(json).unwrap();
        assert_eq!(resp.len(), 2);
        assert_eq!(resp[0].id, "tag-1");
        assert_eq!(resp[0].name, "moments-edit");
    }

    #[test]
    fn client_clone_shares_base_url() {
        let client = ImmichClient::new("https://immich.example.com", "token").unwrap();
        let cloned = client.clone();
        assert_eq!(client.base_url(), cloned.base_url());
    }
}
