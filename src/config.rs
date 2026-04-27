//! Runtime configuration loaded from environment variables.
//!
//! [`Config::from_env`] is called once at startup. Any variable that is
//! present but cannot be parsed causes an immediate panic — Hermes must not
//! start in a misconfigured state.

use std::env;

/// All tunable parameters for a Hermes instance.
///
/// Values are read from environment variables at startup; see [`Config::from_env`].
/// Every field has a compile-time default defined in [`Config::default`].
#[derive(Debug)]
pub struct Config {
    /// TCP port the HTTP server listens on. Env: `PORT`. Default: `3001`.
    pub port: u16,
    /// Redis connection URL. Env: `REDIS_URL`. Default: `redis://127.0.0.1:6379`.
    pub redis_url: String,
    /// `User-Agent` header sent to upstream servers. Env: `USER_AGENT`.
    /// Default: `HermesBot/1.0`.
    pub user_agent: String,
    /// Maximum number of concurrent in-flight requests per domain.
    /// Env: `DOMAIN_CONCURRENCY`. Default: `3`.
    pub domain_concurrency: usize,
    /// Default cache TTL in seconds when `ttl_secs` is not specified in the
    /// request. Env: `DEFAULT_TTL_SECS`. Default: `300`.
    pub default_ttl_secs: u64,
    /// Maximum allowed response body size in bytes. Requests that exceed this
    /// limit return a `502 Body Too Large` error.
    /// Env: `MAX_BODY_BYTES`. Default: `10485760` (10 MiB).
    pub max_body_bytes: usize,
    /// Cloudflare API token for Browser Rendering (Tier 1).
    /// Env: `CF_API_TOKEN`. Optional — browser rendering is disabled if absent.
    pub cf_api_token: Option<String>,
    /// Cloudflare account ID for Browser Rendering (Tier 1).
    /// Env: `CF_ACCOUNT_ID`. Optional — browser rendering is disabled if absent.
    pub cf_account_id: Option<String>,
    /// Optional bearer token for authenticating callers.
    /// Env: `HERMES_API_KEY`. When set, every non-health request must supply
    /// `Authorization: Bearer <key>`. When absent, the endpoints are open.
    ///
    /// **Strongly recommended** whenever Hermes is reachable from untrusted
    /// networks. Always place Hermes behind TLS so the key is not sent in clear
    /// text.
    pub api_key: Option<String>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            port: 3001,
            redis_url: "redis://127.0.0.1:6379".to_string(),
            user_agent: "HermesBot/1.0".to_string(),
            domain_concurrency: 3,
            default_ttl_secs: 300,
            max_body_bytes: 10485760,
            cf_api_token: None,
            cf_account_id: None,
            api_key: None,
        }
    }
}

impl Config {
    /// Build a [`Config`] from environment variables, falling back to
    /// [`Config::default`] for any variable that is not set.
    ///
    /// # Panics
    ///
    /// Panics if an environment variable is present but cannot be parsed into
    /// the expected type (e.g. `PORT=not_a_number`).
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            port: get_env_parse("PORT", d.port),
            redis_url: get_env_parse("REDIS_URL", d.redis_url),
            user_agent: get_env_parse("USER_AGENT", d.user_agent),
            domain_concurrency: get_env_parse("DOMAIN_CONCURRENCY", d.domain_concurrency),
            default_ttl_secs: get_env_parse("DEFAULT_TTL_SECS", d.default_ttl_secs),
            max_body_bytes: get_env_parse("MAX_BODY_BYTES", d.max_body_bytes),
            cf_api_token: env::var("CF_API_TOKEN").ok(),
            cf_account_id: env::var("CF_ACCOUNT_ID").ok(),
            api_key: env::var("HERMES_API_KEY").ok().filter(|k| !k.is_empty()),
        }
    }
}

/// Read `key` from the environment, parsing it into `T`. Returns `default`
/// when the variable is absent; panics when it is present but unparseable.
fn get_env_parse<T>(key: &str, default: T) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Debug,
{
    match env::var(key) {
        Ok(val) => val
            .parse::<T>()
            .unwrap_or_else(|_| panic!("Config error: '{}' has invalid value '{}'", key, val)),
        Err(_) => default,
    }
}
