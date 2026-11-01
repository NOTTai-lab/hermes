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
    /// Leaf certificate the upstream presented on an `https` Tier 0 fetch.
    /// Absent for plain HTTP, browser rendering, or a certificate Hermes
    /// could not parse. Older cached entries have no such field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls: Option<TlsCertificate>,
}

/// Facts about the upstream server's leaf certificate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TlsCertificate {
    /// Start of validity, RFC 3339 UTC.
    pub not_before: String,
    /// End of validity, RFC 3339 UTC.
    pub not_after: String,
    /// Issuer organisation, or its common name when there is none.
    pub issuer: Option<String>,
    /// Subject common name.
    pub subject: Option<String>,
    /// DNS names from the subject alternative name extension (at most 100).
    pub dns_names: Vec<String>,
    /// SHA-256 of the DER certificate, lowercase hex.
    pub sha256: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_entries_without_tls_still_parse() {
        let json = r#"{"status":200,"headers":{},"body":"","body_encoding":"utf8","render":"none","cached":false,"fetched_at":"2026-11-01T00:00:00Z"}"#;
        let response: FetchResponse = serde_json::from_str(json).expect("old shape parses");
        assert!(response.tls.is_none());
        assert!(
            !serde_json::to_string(&response)
                .expect("serialize")
                .contains("\"tls\"")
        );
    }
}
