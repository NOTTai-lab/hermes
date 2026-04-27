//! Tier 0 HTTP fetch — plain `reqwest`-based request execution.
//!
//! [`Fetcher`] owns a shared [`reqwest::Client`] with keep-alive and
//! connection-pooling configured for reuse across requests. Binary response
//! bodies are transparently base64-encoded so they can be transported in the
//! JSON envelope.

use crate::types::{BodyEncoding, FetchRequest, FetchResponse, RenderMode};
use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::{Client, Method};
use std::collections::HashMap;
use std::time::Duration;

/// Tier 0 HTTP fetcher backed by a shared `reqwest` connection pool.
///
/// A single [`Fetcher`] instance is created at startup and shared (via
/// [`std::sync::Arc`]) across all request handlers.
///
/// Two clients are pre-built:
/// - `client` — follows redirects (default).
/// - `no_redirect_client` — redirect policy set to `none`, used when
///   `follow_redirects: false` is requested. Pre-building it avoids
///   per-request allocation and keeps the connection pool warm.
pub struct Fetcher {
    /// The underlying HTTP client with redirect-following enabled.
    /// Also reused by [`crate::robots::RobotsChecker`] and streaming handlers.
    pub client: Client,
    no_redirect_client: Client,
    max_body_bytes: usize,
}

impl Fetcher {
    /// Create a new [`Fetcher`] with `user_agent` and `max_body_bytes`.
    ///
    /// Both the redirect-following and no-redirect clients are configured with:
    /// - Up to 10 idle connections per host.
    /// - 60-second TCP keep-alive.
    ///
    /// # Panics
    ///
    /// Panics when the TLS backend cannot be initialised (should never happen
    /// in a correctly compiled binary).
    pub fn new(user_agent: String, max_body_bytes: usize) -> Self {
        let client = Client::builder()
            .user_agent(&user_agent)
            .pool_max_idle_per_host(10)
            .tcp_keepalive(Duration::from_secs(60))
            .build()
            .expect("failed to build reqwest client");

        let no_redirect_client = Client::builder()
            .user_agent(&user_agent)
            .pool_max_idle_per_host(10)
            .tcp_keepalive(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build no-redirect reqwest client");

        Self {
            client,
            no_redirect_client,
            max_body_bytes,
        }
    }

    /// Execute `req` as a plain HTTP request (Tier 0).
    ///
    /// The method, URL, headers, body, timeout, and redirect policy are all
    /// taken from `req`. Binary response bodies are base64-encoded; UTF-8
    /// bodies are returned as-is.
    ///
    /// # Errors
    ///
    /// Returns an error when:
    /// - `req.method` is not a valid HTTP method token.
    /// - The upstream server cannot be reached or the request times out.
    /// - The response body exceeds [`Fetcher::max_body_bytes`].
    pub async fn execute(&self, req: FetchRequest) -> anyhow::Result<FetchResponse> {
        let method = Method::from_bytes(req.method.to_uppercase().as_bytes())
            .context("invalid HTTP method")?;

        let client = if req.follow_redirects {
            &self.client
        } else {
            &self.no_redirect_client
        };

        let mut builder = client
            .request(method, &req.url)
            .timeout(Duration::from_secs(req.timeout_secs));

        for (k, v) in req.headers {
            builder = builder.header(k, v);
        }

        if let Some(body) = req.body {
            builder = builder.body(body);
        }

        let response = builder.send().await.context("request failed")?;

        let status = response.status().as_u16();

        let response_headers: HashMap<String, String> = response
            .headers()
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|value| (k.to_string(), value.to_string()))
            })
            .collect();

        let bytes = response.bytes().await.context("failed to read body")?;
        if bytes.len() > self.max_body_bytes {
            anyhow::bail!(
                "response body {} bytes exceeds limit of {} bytes",
                bytes.len(),
                self.max_body_bytes
            );
        }

        let (body, body_encoding) = match std::str::from_utf8(&bytes) {
            Ok(s) => (s.to_string(), BodyEncoding::Utf8),
            Err(_) => (STANDARD.encode(&bytes), BodyEncoding::Base64),
        };

        Ok(FetchResponse {
            status,
            headers: response_headers,
            body,
            body_encoding,
            render: RenderMode::None,
            cached: false,
            fetched_at: chrono::Utc::now().to_rfc3339(),
        })
    }
}
