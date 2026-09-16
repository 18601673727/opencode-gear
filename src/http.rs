//! HTTP/download abstraction.
//!
//! The runtime only ever needs two things over the network: a JSON release
//! document and a release archive. Both go through [`HttpTransport`] so tests
//! can run the whole runtime pipeline without touching the network.

use crate::error::{GearError, Result};
use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Upper bound on a single response body (512 MiB).
///
/// Release archives are tens of megabytes; the cap protects against a hostile
/// or broken server that streams without a `Content-Length`.
pub const MAX_BODY_BYTES: u64 = 512 * 1024 * 1024;

/// A minimal blocking HTTP client.
pub trait HttpTransport: Send + Sync {
    /// Fetch a URL and return the raw body.
    fn get(&self, url: &str) -> Result<Vec<u8>>;

    /// Fetch a URL and decode the body as UTF-8.
    fn get_text(&self, url: &str) -> Result<String> {
        let bytes = self.get(url)?;
        String::from_utf8(bytes).map_err(|error| {
            GearError::config(format!("response from {url} is not valid UTF-8: {error}"))
        })
    }
}

/// The production transport: one `reqwest` blocking client with rustls.
///
/// Only `https://` URLs are accepted, and bodies are capped at
/// [`MAX_BODY_BYTES`] whether or not the server sends a `Content-Length`.
pub struct ReqwestHttp {
    client: reqwest::blocking::Client,
}

impl ReqwestHttp {
    pub fn new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("opencode-gear/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|error| GearError::config(format!("cannot build the HTTP client: {error}")))?;
        Ok(Self { client })
    }
}

impl HttpTransport for ReqwestHttp {
    fn get(&self, url: &str) -> Result<Vec<u8>> {
        if !url.starts_with("https://") {
            return Err(GearError::config(format!("refusing non-HTTPS URL: {url}")));
        }
        let response = self
            .client
            .get(url)
            .send()
            .map_err(|error| GearError::config(format!("request to {url} failed: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            return Err(GearError::config(format!(
                "request to {url} failed with HTTP {status}"
            )));
        }
        if let Some(length) = response.content_length() {
            if length > MAX_BODY_BYTES {
                return Err(GearError::config(format!(
                    "response from {url} is {length} bytes, over the {MAX_BODY_BYTES} byte limit"
                )));
            }
        }
        let mut bytes = Vec::new();
        // `take` also enforces the cap when no Content-Length was sent.
        let mut reader = response.take(MAX_BODY_BYTES + 1);
        reader.read_to_end(&mut bytes).map_err(|error| {
            GearError::config(format!("cannot read the response from {url}: {error}"))
        })?;
        if bytes.len() as u64 > MAX_BODY_BYTES {
            return Err(GearError::config(format!(
                "response from {url} exceeded the {MAX_BODY_BYTES} byte limit"
            )));
        }
        Ok(bytes)
    }
}

/// A transport that always fails. Used by non-mutating commands (`version`,
/// `doctor`) that must never touch the network.
#[doc(hidden)]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoHttp;

impl HttpTransport for NoHttp {
    fn get(&self, url: &str) -> Result<Vec<u8>> {
        Err(GearError::config(format!(
            "network access is disabled for this command ({url})"
        )))
    }
}

/// An in-memory transport for tests and offline tooling. It records every
/// requested URL so tests can prove that a cached check makes no request.
#[doc(hidden)]
#[derive(Debug, Default, Clone)]
pub struct MemoryHttp {
    responses: HashMap<String, Vec<u8>>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl MemoryHttp {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, url: &str, body: impl Into<Vec<u8>>) -> Self {
        self.insert(url, body);
        self
    }

    pub fn insert(&mut self, url: &str, body: impl Into<Vec<u8>>) {
        self.responses.insert(url.to_string(), body.into());
    }

    /// URLs requested so far, in order.
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().expect("fake requests").clone()
    }
}

impl HttpTransport for MemoryHttp {
    fn get(&self, url: &str) -> Result<Vec<u8>> {
        self.requests
            .lock()
            .expect("fake requests")
            .push(url.to_string());
        self.responses
            .get(url)
            .cloned()
            .ok_or_else(|| GearError::config(format!("no fixture registered for {url}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_transport_serves_registered_bodies() {
        let http = MemoryHttp::new().with("https://example.test/a", b"hello".to_vec());
        assert_eq!(http.get("https://example.test/a").unwrap(), b"hello");
        assert_eq!(http.get_text("https://example.test/a").unwrap(), "hello");
        assert!(http.get("https://example.test/missing").is_err());
        assert_eq!(
            http.requests(),
            vec![
                "https://example.test/a".to_string(),
                "https://example.test/a".to_string(),
                "https://example.test/missing".to_string(),
            ]
        );
    }

    #[test]
    fn production_transport_rejects_non_https() {
        let http = ReqwestHttp::new().unwrap();
        assert!(http.get("http://example.test/a").is_err());
        assert!(http.get("file:///etc/hosts").is_err());
    }
}
