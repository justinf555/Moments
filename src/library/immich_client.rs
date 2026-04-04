use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use super::error::LibraryError;

/// HTTP client for the Immich server API.
///
/// Uses session-based authentication (`Authorization: Bearer {token}`).
/// The session token is obtained via [`ImmichClient::login`] and stored
/// in the GNOME Keyring. Sessions persist indefinitely on the server.
///
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
                "login failed with status {status}: {body}"
            )));
        }

        let login: LoginResponse = resp
            .json()
            .await
            .map_err(|e| LibraryError::Immich(format!("invalid login response: {e}")))?;

        debug!(user = %login.name, "login successful");
        Ok(login)
    }

    #[allow(dead_code)]
    /// The base server URL (without trailing slash).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Build a full URL for an API endpoint path.
    fn url(&self, path: &str) -> String {
        format!("{}/api{}", self.base_url, path)
    }

    #[allow(dead_code)]
    /// Ping the server to check connectivity.
    ///
    /// Returns `Ok(())` if the server responds with `{"res": "pong"}`.
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

    #[allow(dead_code)]
    /// Validate the connection by pinging and fetching server info.
    ///
    /// Used by the setup wizard to test that the server URL and API key
    /// are correct before creating the library bundle.
    #[instrument(skip(self), fields(url = %self.base_url))]
    pub async fn validate(&self) -> Result<ServerAbout, LibraryError> {
        self.ping().await?;
        self.server_about().await
    }

    // ── Private helpers ────────────────────────────────────────────────────

    /// Send a request, check the status, and return the response.
    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        method: &str,
        path: &str,
    ) -> Result<reqwest::Response, LibraryError> {
        let resp = request
            .send()
            .await
            .map_err(|e| LibraryError::Immich(format!("{method} {path} failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LibraryError::Immich(format!(
                "{method} {path} returned {status}: {body}"
            )));
        }

        Ok(resp)
    }

    /// Send a request, check status, parse JSON response.
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

    /// Send a request, check status, discard body.
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

    pub(crate) async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<T, LibraryError> {
        self.send_json(self.client.get(self.url(path)), "GET", path).await
    }

    pub(crate) async fn post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, LibraryError> {
        self.send_json(self.client.post(self.url(path)).json(body), "POST", path).await
    }

    #[allow(dead_code)]
    pub(crate) async fn put<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, LibraryError> {
        self.send_json(self.client.put(self.url(path)).json(body), "PUT", path).await
    }

    #[allow(dead_code)]
    pub(crate) async fn delete<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<T, LibraryError> {
        self.send_json(self.client.delete(self.url(path)), "DELETE", path).await
    }

    pub(crate) async fn post_no_content<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), LibraryError> {
        self.send_no_content(self.client.post(self.url(path)).json(body), "POST", path).await
    }

    pub(crate) async fn put_no_content<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), LibraryError> {
        self.send_no_content(self.client.put(self.url(path)).json(body), "PUT", path).await
    }

    pub(crate) async fn delete_no_content(&self, path: &str) -> Result<(), LibraryError> {
        self.send_no_content(self.client.delete(self.url(path)), "DELETE", path).await
    }

    pub(crate) async fn delete_with_body<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), LibraryError> {
        self.send_no_content(self.client.delete(self.url(path)).json(body), "DELETE", path).await
    }

    pub(crate) async fn patch_no_content<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), LibraryError> {
        self.send_no_content(self.client.patch(self.url(path)).json(body), "PATCH", path).await
    }

    /// Upload an asset to the Immich server via multipart form-data.
    ///
    /// Returns the server-assigned asset ID and status ("created" or "duplicate").
    pub(crate) async fn upload_asset(
        &self,
        file_path: &std::path::Path,
        device_asset_id: &str,
        file_created_at: &str,
        file_modified_at: &str,
        checksum: Option<&str>,
    ) -> Result<UploadResponse, LibraryError> {
        let url = self.url("/assets");

        let file_name = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("upload")
            .to_owned();

        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(LibraryError::Io)?;

        let file_part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name)
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
        // 201 = created, 200 = duplicate
        if !status_code.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LibraryError::Immich(format!(
                "upload returned {status_code}: {body}"
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

        let id = body["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();

        Ok(UploadResponse {
            id,
            status: upload_status,
        })
    }

    /// Make a GET request and return the raw response bytes.
    pub(crate) async fn get_bytes(&self, path: &str) -> Result<Vec<u8>, LibraryError> {
        let resp = self.send(self.client.get(self.url(path)), "GET", path).await?;
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| LibraryError::Immich(format!("GET {path} read failed: {e}")))
    }

    /// Send a POST request and return the raw response for streaming.
    pub(crate) async fn post_stream<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<reqwest::Response, LibraryError> {
        self.send(self.client.post(self.url(path)).json(body), "POST", path).await
    }
}

#[derive(Debug, Serialize)]
struct LoginRequest {
    email: String,
    password: String,
}

/// Response from `POST /auth/login`.
#[derive(Debug, Clone, Deserialize)]
pub struct LoginResponse {
    /// Session token — use as `Authorization: Bearer {access_token}`.
    #[serde(rename = "accessToken")]
    pub access_token: String,
    #[allow(dead_code)]
    /// Immich user ID (UUID).
    #[serde(rename = "userId")]
    pub user_id: String,
    /// Display name of the authenticated user.
    pub name: String,
}

/// Response from `POST /assets` (upload).
#[derive(Debug, Clone)]
pub struct UploadResponse {
    /// Server-assigned asset UUID.
    pub id: String,
    /// "created" or "duplicate".
    pub status: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct PingResponse {
    res: String,
}

/// Server version and build information from `GET /server/about`.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerAbout {
    pub version: String,
    #[allow(dead_code)]
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
}
