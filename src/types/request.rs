//! Request types for the Hermes fetch API.
//!
//! Defines the input structures accepted by the `/fetch` and `/fetch/stream`
//! endpoints, including the render tier selection and all per-request options.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Selects which fetch tier to use for a request.
///
/// Each tier trades latency and cost for the ability to execute JavaScript or
/// route through residential IP addresses.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "lowercase")]
pub enum RenderMode {
    /// Tier 0 — plain HTTP via `reqwest`. Fastest and free; cannot render JS.
    #[default]
    None,
    /// Tier 1 — Cloudflare Browser Rendering. Executes JavaScript; requires
    /// `CF_API_TOKEN` and `CF_ACCOUNT_ID` to be set.
    Browser,
    /// Tier 2 — residential proxy. Reserved for v2; returns `501 Not Implemented`.
    Proxy,
}

/// Format in which the rendered page content is returned when using
/// [`RenderMode::Browser`].
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "lowercase")]
pub enum RenderOutput {
    /// Return the full HTML of the page (default).
    #[default]
    Html,
    /// Return a Markdown representation of the page content.
    Markdown,
    /// Return structured JSON — requires a schema; reserved for v2.
    Json,
}

/// The full set of parameters accepted by `POST /fetch` and `POST /fetch/stream`.
///
/// All fields except `url` are optional and have sensible defaults.
///
/// # Examples
///
/// ```json
/// {
///   "url": "https://example.com",
///   "method": "GET",
///   "ttl_secs": 300,
///   "ignore_robots": false,
///   "crawl_delay_ms": 0
/// }
/// ```
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct FetchRequest {
    /// Target URL to fetch. Required — no default.
    pub url: String,
    /// HTTP method (e.g. `"GET"`, `"POST"`). Case-insensitive. Default: `"GET"`.
    pub method: String,
    /// Additional HTTP request headers forwarded verbatim to the upstream server.
    pub headers: HashMap<String, String>,
    /// Raw request body. Only relevant for methods that carry a payload (e.g. POST).
    pub body: Option<String>,
    /// Fetch tier to use. Default: [`RenderMode::None`] (plain HTTP).
    pub render: RenderMode,
    /// Output format for browser-rendered responses. Default: [`RenderOutput::Html`].
    pub render_output: RenderOutput,
    /// Cache TTL in seconds. Set to `0` to bypass the cache entirely. Default: `300`.
    pub ttl_secs: u64,
    /// When `true`, skip the robots.txt compliance check. Default: `false`.
    pub ignore_robots: bool,
    /// Artificial delay in milliseconds applied before the request is sent.
    /// Useful for polite crawling. Default: `0`.
    pub crawl_delay_ms: u64,
    /// Whether to follow HTTP redirects. Default: `true`.
    pub follow_redirects: bool,
    /// Per-request timeout in seconds. Default: `30`.
    pub timeout_secs: u64,
}

impl Default for FetchRequest {
    fn default() -> Self {
        FetchRequest {
            url: String::new(),
            method: "GET".to_string(),
            headers: HashMap::new(),
            body: None,
            render: RenderMode::None,
            render_output: RenderOutput::Html,
            ttl_secs: 300,
            ignore_robots: false,
            crawl_delay_ms: 0,
            follow_redirects: true,
            timeout_secs: 30,
        }
    }
}
