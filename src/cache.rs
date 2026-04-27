//! Redis-backed response cache.
//!
//! Responses are stored as JSON strings keyed by a deterministic SHA-256 hash
//! of the request method, URL, render mode, and sorted headers. Cache misses —
//! including Redis connection failures — are transparent to the caller: the
//! request is simply re-executed and the result cached on the way out.
//!
//! A [`redis::aio::ConnectionManager`] is used so that a single multiplexed
//! connection is reused across all concurrent requests, with automatic
//! reconnection on failure.

use crate::types::FetchResponse;
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Redis-backed cache for [`FetchResponse`] values.
///
/// A single [`Cache`] instance is shared across all request handlers via
/// [`std::sync::Arc`]. The internal [`ConnectionManager`] multiplexes all
/// cache operations over a single long-lived Redis connection; cloning it is
/// cheap (reference-counted).
pub struct Cache {
    conn: ConnectionManager,
}

impl Cache {
    /// Connect to Redis at `redis_url` and return a new [`Cache`].
    ///
    /// Establishes the multiplexed connection eagerly so connectivity problems
    /// are surfaced at startup rather than on the first request.
    ///
    /// # Errors
    ///
    /// Returns an error when `redis_url` is invalid or the initial connection
    /// cannot be established.
    pub async fn new(redis_url: &str) -> anyhow::Result<Self> {
        let client = redis::Client::open(redis_url)
            .map_err(|e| anyhow::anyhow!("invalid Redis URL {}: {}", redis_url, e))?;
        let conn = ConnectionManager::new(client)
            .await
            .map_err(|e| anyhow::anyhow!("failed to connect to Redis at {}: {}", redis_url, e))?;
        Ok(Self { conn })
    }

    /// Compute a deterministic cache key for the given request parameters.
    ///
    /// The key is `hermes:v2:<sha256>` where the hash covers:
    /// - `method` uppercased
    /// - `url` verbatim
    /// - `render` tier string (`"none"`, `"browser"`, `"proxy"`)
    /// - all `headers` sorted by key name (key and value concatenated)
    ///
    /// The `v2` prefix ensures old `v1` entries (which did not include the
    /// render tier) are never accidentally returned.
    pub fn cache_key(
        method: &str,
        url: &str,
        headers: &HashMap<String, String>,
        render: &str,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(method.to_uppercase().as_bytes());
        hasher.update(url.as_bytes());
        hasher.update(render.as_bytes());

        let mut pairs: Vec<_> = headers.iter().collect();
        pairs.sort_by_key(|&(k, _)| k);

        for (k, v) in pairs {
            hasher.update(k.as_bytes());
            hasher.update(v.as_bytes());
        }

        let digest = hasher.finalize();
        format!("hermes:v2:{}", hex::encode(digest))
    }

    /// Retrieve a cached [`FetchResponse`] by `key`.
    ///
    /// Returns `None` on a cache miss or any error (connection failure,
    /// deserialization failure). All errors are logged at `WARN` level.
    pub async fn get(&self, key: &str) -> Option<FetchResponse> {
        let mut conn = self.conn.clone();

        let value: String = match conn.get(key).await {
            Ok(v) => v,
            Err(_) => return None,
        };

        match serde_json::from_str(&value) {
            Ok(response) => Some(response),
            Err(e) => {
                tracing::warn!("cache: failed to deserialize value for key {}: {}", key, e);
                None
            }
        }
    }

    /// Persist `response` in Redis under `key` with the given TTL.
    ///
    /// A `ttl_secs` of `0` is a no-op. Write failures are logged at `WARN`
    /// level and never propagate to the caller.
    pub async fn set(&self, key: &str, response: &FetchResponse, ttl_secs: u64) {
        if ttl_secs == 0 {
            return;
        }

        let mut conn = self.conn.clone();

        let serialized = match serde_json::to_string(response) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("cache set failed (serialize): {e}");
                return;
            }
        };

        if let Err(e) = conn.set_ex::<_, _, ()>(key, serialized, ttl_secs).await {
            tracing::warn!("cache set failed (redis): {e}");
        }
    }
}
