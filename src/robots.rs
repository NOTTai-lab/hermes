//! Robots.txt compliance checking with Redis caching.
//!
//! [`RobotsChecker`] fetches and caches each domain's `robots.txt` for 24
//! hours. The checker **fails open**: if the robots.txt cannot be fetched or
//! parsed for any reason, the request is allowed to proceed.
//!
//! A [`redis::aio::ConnectionManager`] is used for connection reuse and
//! automatic reconnection.

use anyhow::Context;
use redis::AsyncCommands;
use redis::aio::ConnectionManager;

/// Checks whether a URL may be crawled according to the site's `robots.txt`.
///
/// Robots.txt files are fetched once per domain (including port) and cached in
/// Redis for 24 hours (`86400` seconds). A single [`RobotsChecker`] instance
/// is shared across all request handlers via [`std::sync::Arc`].
pub struct RobotsChecker {
    conn: ConnectionManager,
    http: reqwest::Client,
    user_agent: String,
}

impl RobotsChecker {
    /// Create a new [`RobotsChecker`].
    ///
    /// `redis_url` is used exclusively for caching robots.txt documents.
    /// `http` is the shared reqwest client (reuses the [`crate::fetch::Fetcher`]
    /// connection pool). `user_agent` is the string checked against `User-agent`
    /// directives in robots.txt.
    ///
    /// # Errors
    ///
    /// Returns an error if `redis_url` is invalid or the initial Redis
    /// connection cannot be established.
    pub async fn new(
        redis_url: &str,
        http: reqwest::Client,
        user_agent: String,
    ) -> anyhow::Result<Self> {
        let client = redis::Client::open(redis_url).context("robots checker: invalid Redis URL")?;
        let conn = ConnectionManager::new(client)
            .await
            .context("robots checker: failed to connect to Redis")?;

        Ok(Self {
            conn,
            http,
            user_agent,
        })
    }

    /// Return `true` if `url` is allowed by the site's `robots.txt`, or if
    /// the check cannot be performed (fail-open policy).
    ///
    /// Errors are logged at `WARN` level and result in `true` being returned.
    pub async fn is_allowed(&self, url: &str) -> bool {
        match self.check(url).await {
            Ok(allowed) => allowed,
            Err(e) => {
                tracing::warn!("robots check failed for {}, allowing: {}", url, e);
                true
            }
        }
    }

    /// Internal implementation — parses `url`, fetches (or cache-hits) the
    /// corresponding `robots.txt`, and evaluates the rules.
    async fn check(&self, url: &str) -> anyhow::Result<bool> {
        let parsed = url::Url::parse(url)?;

        // Use the URL origin so that non-standard ports are included in both
        // the robots.txt fetch URL and the Redis cache key.
        // e.g. http://example.com:8080  →  http://example.com:8080/robots.txt
        let origin = parsed.origin().ascii_serialization();
        let robots_url = format!("{}/robots.txt", origin);
        let cache_key = format!("hermes:robots:v1:{}", origin);

        let content = self.fetch_robots_txt(&robots_url, &cache_key).await?;
        let robot = texting_robots::Robot::new(&self.user_agent, content.as_bytes())?;
        Ok(robot.allowed(parsed.as_str()))
    }

    /// Fetch `robots_url`, returning the cached value from Redis when available.
    ///
    /// Network errors and non-200/404 status codes are treated as an empty
    /// robots.txt (allow all). A successful fetch is written to Redis with a
    /// 24-hour TTL; write failures are silently ignored.
    async fn fetch_robots_txt(&self, robots_url: &str, cache_key: &str) -> anyhow::Result<String> {
        let mut conn = self.conn.clone();

        if let Ok(cached) = conn.get::<_, String>(cache_key).await {
            return Ok(cached);
        }

        let response = match self
            .http
            .get(robots_url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(_) => return Ok(String::new()),
        };

        let robots_txt = match response.status().as_u16() {
            200 => response.text().await?,
            _ => String::new(),
        };

        let _ = conn.set_ex::<_, _, ()>(cache_key, &robots_txt, 86400).await;

        Ok(robots_txt)
    }
}
