//! Shared utility types used across multiple Hermes endpoints.

use serde::Serialize;

/// Response body for `GET /health`.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// Always `"ok"` when the service is running.
    pub status: &'static str,
    /// Semver version string taken from `Cargo.toml` at compile time.
    pub version: &'static str,
}
