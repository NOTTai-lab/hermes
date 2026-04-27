//! Response types returned by the Hermes fetch API.

use crate::types::RenderMode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Indicates how the response body is encoded in the JSON payload.
///
/// Binary responses (non-UTF-8 bytes) are automatically base64-encoded so they
/// can be transported safely in JSON strings.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyEncoding {
    /// Body is a valid UTF-8 string returned as-is.
    Utf8,
    /// Body has been base64-encoded because the raw bytes are not valid UTF-8.
    Base64,
}

/// The response payload returned by `POST /fetch`.
///
/// Contains the upstream HTTP status, headers, body, and Hermes-specific
/// metadata such as whether the response was served from cache and which
/// render tier was used.
#[derive(Debug, Serialize, Deserialize)]
pub struct FetchResponse {
    /// HTTP status code returned by the upstream server (e.g. `200`, `404`).
    pub status: u16,
    /// Response headers forwarded from the upstream server.
    pub headers: HashMap<String, String>,
    /// Response body. Encoding is indicated by [`body_encoding`](Self::body_encoding).
    pub body: String,
    /// Encoding used for [`body`](Self::body).
    pub body_encoding: BodyEncoding,
    /// Fetch tier that produced this response.
    pub render: RenderMode,
    /// `true` when this response was served from the Redis cache.
    pub cached: bool,
    /// RFC 3339 timestamp of when the request was originally executed.
    pub fetched_at: String,
}
