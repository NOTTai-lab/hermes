//! Axum HTTP handlers and shared application state.
//!
//! Three endpoints are exposed:
//! - `GET /health` — liveness check (no auth required).
//! - `POST /fetch` — full pipeline: auth → validate → cache → robots → delay → limiter → fetch → cache.
//! - `POST /fetch/stream` — streaming pipeline: auth → validate → robots → delay → limiter → stream.

use crate::types::{FetchRequest, FetchResponse, HealthResponse, RenderMode};
use crate::validate::validate_request;
use crate::{BrowserRenderer, Cache, DomainLimiter, Fetcher, RobotsChecker};
use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use subtle::ConstantTimeEq;

/// Shared state injected into every request handler by Axum's [`State`] extractor.
///
/// All fields are reference-counted so the state is cheap to clone per-request.
#[derive(Clone)]
pub struct AppState {
    /// Redis-backed response cache.
    pub cache: Arc<Cache>,
    /// Tier 0 plain-HTTP fetcher.
    pub fetcher: Arc<Fetcher>,
    /// Optional Tier 1 browser renderer (requires Cloudflare credentials).
    pub renderer: Option<Arc<BrowserRenderer>>,
    /// Per-domain concurrency limiter.
    pub limiter: Arc<DomainLimiter>,
    /// Robots.txt compliance checker.
    pub robots: Arc<RobotsChecker>,
    /// Optional bearer token. When `Some`, all non-health requests must supply
    /// `Authorization: Bearer <token>`. Comparisons are constant-time.
    pub api_key: Option<String>,
}

/// Unified error type for all handler failures.
///
/// Each variant maps to a specific HTTP status code and a machine-readable
/// error `code` string included in the JSON response body.
pub enum AppError {
    /// Upstream fetch failed (network error, timeout, body too large, etc.).
    /// Maps to `502 Bad Gateway` with code `FETCH_FAILED`.
    FetchFailed(String),
    /// The target URL is disallowed by its `robots.txt`.
    /// Maps to `403 Forbidden` with code `FORBIDDEN`.
    Forbidden(String),
    /// The request payload is malformed or contains an invalid field value.
    /// Maps to `400 Bad Request` with code `BAD_REQUEST`.
    BadRequest(String),
    /// The requested feature is not yet implemented (e.g. Tier 2 proxy).
    /// Maps to `501 Not Implemented` with code `NOT_IMPLEMENTED`.
    NotImplemented(String),
    /// The upstream response body exceeds [`crate::config::Config::max_body_bytes`].
    /// Maps to `502 Bad Gateway` with code `BODY_TOO_LARGE`.
    BodyTooLarge(String),
    /// Missing or invalid `Authorization` header.
    /// Maps to `401 Unauthorized` with code `UNAUTHORIZED`.
    Unauthorized,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, msg) = match self {
            AppError::FetchFailed(msg) => (StatusCode::BAD_GATEWAY, "FETCH_FAILED", msg),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, "FORBIDDEN", msg),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", msg),
            AppError::NotImplemented(msg) => (StatusCode::NOT_IMPLEMENTED, "NOT_IMPLEMENTED", msg),
            AppError::BodyTooLarge(msg) => (StatusCode::BAD_GATEWAY, "BODY_TOO_LARGE", msg),
            AppError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "valid Bearer token required".to_string(),
            ),
        };

        let body = json!({
            "error": msg,
            "code": code
        });

        (status, Json(body)).into_response()
    }
}

/// Verify the `Authorization: Bearer <token>` header against the configured
/// API key. Comparison is constant-time to prevent timing attacks.
///
/// Returns `Ok(())` when auth is disabled (`api_key` is `None`).
fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let Some(ref expected) = state.api_key else {
        return Ok(());
    };

    let provided = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    // Lengths must match first (no-branch check); then compare content.
    let len_ok = provided.len() == expected.len();
    let content_ok = bool::from(provided.as_bytes().ct_eq(expected.as_bytes()));

    if len_ok && content_ok {
        Ok(())
    } else {
        Err(AppError::Unauthorized)
    }
}

/// `GET /health` — liveness probe.
///
/// Returns `200 OK` with the service status and current server version.
/// Authentication is intentionally not required so that load balancers and
/// container orchestrators can reach this endpoint without credentials.
pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// `POST /fetch` — execute a fetch request through the full Hermes pipeline.
///
/// # Pipeline
///
/// 1. **Auth check** — reject if a valid Bearer token is required but absent.
/// 2. **Request validation** — reject invalid URLs, SSRF targets, out-of-range fields.
/// 3. **Cache lookup** — return cached response if available and `ttl_secs > 0`.
/// 4. **Robots.txt check** — reject if disallowed (unless `ignore_robots: true`).
/// 5. **Crawl delay** — sleep for `crawl_delay_ms` milliseconds.
/// 6. **Domain semaphore** — acquire a per-hostname concurrency permit.
/// 7. **Fetch execution** — dispatch to the appropriate tier (Tier 0, 1, or 2).
/// 8. **Cache write** — store the response if `ttl_secs > 0`.
/// 9. **Return** — send the [`FetchResponse`] to the caller.
///
/// # Errors
///
/// Returns an [`AppError`] when auth fails, the URL is invalid, robots.txt
/// forbids the URL, the upstream fetch fails, or the requested tier is not
/// available / not implemented.
pub async fn fetch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<FetchRequest>,
) -> Result<Json<FetchResponse>, AppError> {
    check_auth(&state, &headers)?;
    validate_request(&req).map_err(AppError::BadRequest)?;

    let render_str = render_key(&req.render);
    let cache_key = Cache::cache_key(&req.method, &req.url, &req.headers, render_str);

    if req.ttl_secs > 0
        && let Some(mut cached) = state.cache.get(&cache_key).await
    {
        cached.cached = true;
        return Ok(Json(cached));
    }

    if !req.ignore_robots && !state.robots.is_allowed(&req.url).await {
        return Err(AppError::Forbidden(format!(
            "robots.txt disallows crawling {}",
            req.url
        )));
    }

    if req.crawl_delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(req.crawl_delay_ms)).await;
    }

    let _permit = state.limiter.acquire(&req.url).await;

    let response = match req.render {
        RenderMode::None => state.fetcher.execute(req.clone()).await.map_err(|e| {
            if e.to_string().contains("exceeds limit") {
                AppError::BodyTooLarge(e.to_string())
            } else {
                AppError::FetchFailed(e.to_string())
            }
        })?,
        RenderMode::Browser => match &state.renderer {
            Some(renderer) => renderer
                .execute(&req)
                .await
                .map_err(|e| AppError::FetchFailed(e.to_string()))?,
            None => {
                return Err(AppError::NotImplemented(
                    "Browser rendering is not configured. Set CF_API_TOKEN and CF_ACCOUNT_ID."
                        .into(),
                ));
            }
        },
        RenderMode::Proxy => {
            return Err(AppError::NotImplemented(
                "Proxy tier is not implemented in v1.".into(),
            ));
        }
    };

    if req.ttl_secs > 0 {
        state.cache.set(&cache_key, &response, req.ttl_secs).await;
    }

    Ok(Json(response))
}

/// `POST /fetch/stream` — stream the upstream response body directly to the caller.
///
/// Unlike [`fetch`], this handler does **not** buffer the body, apply caching,
/// or enforce the `max_body_bytes` limit. It is intended for SSE feeds, large
/// file downloads, and any other streaming use case where buffering would be
/// undesirable.
///
/// The full safety pipeline (auth → validate → robots → crawl delay → domain
/// semaphore) is still applied so that streaming requests are subject to the
/// same access controls as buffered ones.
///
/// # Errors
///
/// Returns an [`AppError`] when auth fails, the URL is invalid, robots.txt
/// forbids the URL, `req.method` is invalid, or the upstream connection fails.
pub async fn fetch_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<FetchRequest>,
) -> Result<Response, AppError> {
    check_auth(&state, &headers)?;
    validate_request(&req).map_err(AppError::BadRequest)?;

    if !req.ignore_robots && !state.robots.is_allowed(&req.url).await {
        return Err(AppError::Forbidden(format!(
            "robots.txt disallows crawling {}",
            req.url
        )));
    }

    if req.crawl_delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(req.crawl_delay_ms)).await;
    }

    let _permit = state.limiter.acquire(&req.url).await;

    let method = reqwest::Method::from_bytes(req.method.to_uppercase().as_bytes())
        .map_err(|_| AppError::BadRequest(format!("invalid method: {}", req.method)))?;

    let mut builder = state
        .fetcher
        .client
        .request(method, &req.url)
        .timeout(Duration::from_secs(req.timeout_secs));

    for (k, v) in &req.headers {
        builder = builder.header(k, v);
    }

    if let Some(body) = &req.body {
        builder = builder.body(body.clone());
    }

    let upstream = builder
        .send()
        .await
        .map_err(|e| AppError::FetchFailed(e.to_string()))?;

    let status = axum::http::StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);

    let mut resp_headers = axum::http::HeaderMap::new();
    for header_name in &[
        "content-type",
        "content-encoding",
        "transfer-encoding",
        "cache-control",
    ] {
        if let Ok(name) = axum::http::HeaderName::from_bytes(header_name.as_bytes())
            && let Some(value) = upstream.headers().get(&name)
            && let Ok(val) = axum::http::HeaderValue::from_bytes(value.as_bytes())
        {
            resp_headers.insert(name, val);
        }
    }

    let body = axum::body::Body::from_stream(upstream.bytes_stream());
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = resp_headers;

    Ok(response)
}

/// Return a stable lowercase string for a render mode, used in cache key hashing.
fn render_key(mode: &RenderMode) -> &'static str {
    match mode {
        RenderMode::None => "none",
        RenderMode::Browser => "browser",
        RenderMode::Proxy => "proxy",
    }
}
