use reqwest::header::{HeaderMap, HeaderValue};
use serde::Deserialize;
use tracing::{debug, instrument};

use super::error::LibraryError;

/// HTTP client for the Immich server API.
///
/// Handles authentication, request building, and response parsing.
/// All methods are async and intended to run on the Tokio executor.
#[derive(Clone)]
pub struct ImmichClient {
    client: reqwest::Client,
    base_url: String,
}

impl ImmichClient {
    /// Create a new client targeting `server_url` with the given API key.
    ///
    /// The `server_url` should be the root URL (e.g. `https://immich.example.com`).
    /// A trailing `/api` is appended automatically for endpoint calls.
    pub fn new(server_url: &str, api_key: &str) -> Result<Self, LibraryError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(api_key)
                .map_err(|e| LibraryError::Immich(format!("invalid API key: {e}")))?,
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

    /// The base server URL (without trailing slash).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Build a full URL for an API endpoint path.
    fn url(&self, path: &str) -> String {
        format!("{}/api{}", self.base_url, path)
    }

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

    /// Validate the connection by pinging and fetching server info.
    ///
    /// Used by the setup wizard to test that the server URL and API key
    /// are correct before creating the library bundle.
    #[instrument(skip(self), fields(url = %self.base_url))]
    pub async fn validate(&self) -> Result<ServerAbout, LibraryError> {
        self.ping().await?;
        self.server_about().await
    }

    /// Make a GET request to the given API path and deserialize the response.
    pub(crate) async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<T, LibraryError> {
        let url = self.url(path);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| LibraryError::Immich(format!("GET {path} failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LibraryError::Immich(format!(
                "GET {path} returned {status}: {body}"
            )));
        }

        resp.json()
            .await
            .map_err(|e| LibraryError::Immich(format!("GET {path} parse failed: {e}")))
    }

    /// Make a POST request with a JSON body and deserialize the response.
    pub(crate) async fn post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, LibraryError> {
        let url = self.url(path);
        let resp = self
            .client
            .post(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| LibraryError::Immich(format!("POST {path} failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LibraryError::Immich(format!(
                "POST {path} returned {status}: {body}"
            )));
        }

        resp.json()
            .await
            .map_err(|e| LibraryError::Immich(format!("POST {path} parse failed: {e}")))
    }

    /// Make a PUT request with a JSON body and deserialize the response.
    pub(crate) async fn put<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, LibraryError> {
        let url = self.url(path);
        let resp = self
            .client
            .put(&url)
            .json(body)
            .send()
            .await
            .map_err(|e| LibraryError::Immich(format!("PUT {path} failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LibraryError::Immich(format!(
                "PUT {path} returned {status}: {body}"
            )));
        }

        resp.json()
            .await
            .map_err(|e| LibraryError::Immich(format!("PUT {path} parse failed: {e}")))
    }

    /// Make a DELETE request and deserialize the response.
    pub(crate) async fn delete<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<T, LibraryError> {
        let url = self.url(path);
        let resp = self
            .client
            .delete(&url)
            .send()
            .await
            .map_err(|e| LibraryError::Immich(format!("DELETE {path} failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LibraryError::Immich(format!(
                "DELETE {path} returned {status}: {body}"
            )));
        }

        resp.json()
            .await
            .map_err(|e| LibraryError::Immich(format!("DELETE {path} parse failed: {e}")))
    }

    /// Make a DELETE request that returns no body (204 No Content).
    pub(crate) async fn delete_no_content(&self, path: &str) -> Result<(), LibraryError> {
        let url = self.url(path);
        let resp = self
            .client
            .delete(&url)
            .send()
            .await
            .map_err(|e| LibraryError::Immich(format!("DELETE {path} failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LibraryError::Immich(format!(
                "DELETE {path} returned {status}: {body}"
            )));
        }

        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct PingResponse {
    res: String,
}

/// Server version and build information from `GET /server/about`.
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
        let client = ImmichClient::new("https://immich.example.com/", "test-key").unwrap();
        assert_eq!(client.base_url(), "https://immich.example.com");
    }

    #[test]
    fn client_preserves_url_without_trailing_slash() {
        let client = ImmichClient::new("https://immich.example.com", "test-key").unwrap();
        assert_eq!(client.base_url(), "https://immich.example.com");
    }

    #[test]
    fn url_builds_api_path() {
        let client = ImmichClient::new("https://immich.example.com", "test-key").unwrap();
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
