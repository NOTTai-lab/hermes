//! Per-domain concurrency limiter.
//!
//! [`DomainLimiter`] prevents Hermes from hammering any single external host
//! with too many simultaneous requests. Each hostname gets its own
//! [`tokio::sync::Semaphore`]; callers block on [`DomainLimiter::acquire`]
//! until a permit is available and hold it for the duration of the request.

use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Enforces a configurable cap on concurrent in-flight requests per hostname.
///
/// Semaphores are created lazily on the first request to a domain. The
/// [`DashMap`] backing store is lock-free for reads, making lookups in the
/// hot path very cheap.
///
/// A single [`DomainLimiter`] is shared across all request handlers via
/// [`std::sync::Arc`].
pub struct DomainLimiter {
    map: DashMap<String, Arc<Semaphore>>,
    max_per_domain: usize,
}

impl DomainLimiter {
    /// Create a new [`DomainLimiter`] that allows at most `max_per_domain`
    /// concurrent requests to any single hostname.
    pub fn new(max_per_domain: usize) -> Self {
        Self {
            map: DashMap::new(),
            max_per_domain,
        }
    }

    /// Extract the lowercase hostname from `url`.
    ///
    /// Falls back to the raw `url` string when parsing fails so that requests
    /// to unparseable URLs still get rate-limited rather than bypassing the
    /// limiter entirely.
    fn host(url: &str) -> String {
        let parsed = match url::Url::parse(url) {
            Ok(u) => u,
            Err(_) => return url.to_string(),
        };

        match parsed.host_str() {
            Some(h) => h.to_lowercase(),
            None => url.to_string(),
        }
    }

    /// Acquire a concurrency permit for the hostname of `url`.
    ///
    /// If `max_per_domain` permits are already in use, this method suspends
    /// the caller until one is released. The returned [`OwnedSemaphorePermit`]
    /// should be held for the lifetime of the upstream request.
    ///
    /// # Panics
    ///
    /// Panics if the semaphore has been closed, which cannot happen in normal
    /// operation.
    pub async fn acquire(&self, url: &str) -> OwnedSemaphorePermit {
        let host = Self::host(url);

        let semaphore = self
            .map
            .entry(host)
            .or_insert_with(|| Arc::new(Semaphore::new(self.max_per_domain)))
            .clone();

        semaphore.acquire_owned().await.expect("semaphore closed")
    }
}
