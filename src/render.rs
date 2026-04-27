//! Tier 1 fetch — Cloudflare Browser Rendering integration.
//!
//! [`BrowserRenderer`] calls the Cloudflare `/browser-rendering/scrape` API to
//! fetch pages in a headless browser. This is required for sites that rely
//! heavily on JavaScript to render their content.
//!
//! Browser rendering is opt-in: [`BrowserRenderer`] is only constructed when
//! `CF_API_TOKEN` and `CF_ACCOUNT_ID` are both present in the environment.

use crate::types::{BodyEncoding, FetchRequest, FetchResponse, RenderMode, RenderOutput};
use anyhow::Context;
use reqwest::Client;
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;

/// Tier 1 fetcher that renders pages via the Cloudflare Browser Rendering API.
///
/// One instance is created at startup (if Cloudflare credentials are present)
/// and shared across all request handlers via [`std::sync::Arc`].
pub struct BrowserRenderer {
    client: Client,
    api_token: String,
    account_id: String,
}

impl BrowserRenderer {
    /// Create a new [`BrowserRenderer`] authenticated with `api_token` and
    /// scoped to `account_id`.
    ///
    /// The internal HTTP client uses a 60-second timeout and a pool of up to
    /// 5 idle connections.
    ///
    /// # Panics
    ///
    /// Panics when the TLS backend cannot be initialised.
    pub fn new(api_token: String, account_id: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(5)
            .build()
            .expect("failed to build browser renderer client");

        Self {
            client,
            api_token,
            account_id,
        }
    }

    /// Render `req.url` using the Cloudflare Browser Rendering API and return
    /// the result as a [`FetchResponse`] with [`RenderMode::Browser`].
    ///
    /// Resource types `image`, `media`, and `font` are suppressed to speed up
    /// rendering. The browser waits for `networkidle0` (500 ms of silence)
    /// before extracting content.
    ///
    /// The output format is governed by [`FetchRequest::render_output`]:
    /// - [`RenderOutput::Html`] — full page HTML via an `html` element selector.
    /// - [`RenderOutput::Markdown`] — Markdown conversion of the page body.
    /// - [`RenderOutput::Json`] — not yet supported; returns an error.
    ///
    /// # Errors
    ///
    /// Returns an error when:
    /// - The Cloudflare API returns a non-2xx status.
    /// - The API response indicates `"success": false`.
    /// - `render_output` is [`RenderOutput::Json`].
    /// - The response body cannot be parsed as JSON.
    pub async fn execute(&self, req: &FetchRequest) -> anyhow::Result<FetchResponse> {
        let api_url = format!(
            "https://api.cloudflare.com/client/v4/accounts/{}/browser-rendering/scrape",
            self.account_id
        );

        let mut body = json!({
            "url": req.url,
            "rejectResourceTypes": ["image", "media", "font"],
            "gotoOptions": {
                "waitUntil": "networkidle0",
                "timeout": req.timeout_secs * 1000
            }
        });

        match req.render_output {
            RenderOutput::Html => {
                body["elements"] = json!([{"selector": "html"}]);
            }
            RenderOutput::Markdown => {
                body["markdownContent"] = json!(true);
            }
            RenderOutput::Json => {
                return Err(anyhow::anyhow!(
                    "JSON render output requires a schema — not yet supported"
                ));
            }
        }

        let response = self
            .client
            .post(&api_url)
            .header("Authorization", format!("Bearer {}", self.api_token))
            .header("Content-Type", "application/json")
            .body(serde_json::to_string(&body)?)
            .send()
            .await
            .context("Cloudflare Browser Rendering request failed")?;

        let status_code = response.status();
        if !status_code.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "Cloudflare API error {}: {}",
                status_code,
                body_text
            ));
        }

        let api_response: serde_json::Value = serde_json::from_str(
            &response
                .text()
                .await
                .context("failed to read response body")?,
        )
        .context("failed to parse Cloudflare API response")?;

        if !api_response["success"].as_bool().unwrap_or(false) {
            let error_msg = api_response["errors"]
                .get(0)
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");

            return Err(anyhow::anyhow!("Cloudflare API error: {}", error_msg));
        }

        let content = api_response["result"]["content"]
            .as_str()
            .context("missing content in Cloudflare response")?
            .to_string();

        let response_headers: HashMap<String, String> = api_response["result"]["headers"]
            .as_object()
            .map(|headers_obj| {
                headers_obj
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|val| (k.clone(), val.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        Ok(FetchResponse {
            status: api_response["result"]["status"].as_u64().unwrap_or(200) as u16,
            headers: response_headers,
            body: content,
            body_encoding: BodyEncoding::Utf8,
            render: RenderMode::Browser,
            cached: false,
            fetched_at: chrono::Utc::now().to_rfc3339(),
        })
    }
}
