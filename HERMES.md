# Hermes — Unified HTTP Egress Gateway
### Implementation Specification

---

## What is Hermes

Hermes is a standalone Rust microservice — the single outbound HTTP gateway for the entire NOTT platform. Every module, service, or component that needs to fetch anything from the external web goes through Hermes. HTML pages, JSON APIs, binary files, robots.txt, sitemaps, llm.txt, images, SSE streams — anything that travels over HTTP outbound is Hermes's responsibility.

The service has zero opinion about content type or purpose. It fetches bytes, optionally renders JavaScript, optionally caches the result, and returns. That is its entire contract.

---

## Why it exists

- **Single outbound identity** — one IP, one User-Agent visible to the external web. All crawling activity from all modules looks like one consistent, well-behaved bot in external server logs and analytics.
- **Caching layer** — identical requests within a TTL window hit Redis instead of the network. Critical for geo/SEO modules that repeatedly check the same targets.
- **Per-domain concurrency control** — prevents accidentally hammering a single host with parallel requests, which would cause IP bans.
- **Robots.txt compliance** — checked automatically before every fetch unless explicitly skipped. Keeps the crawler well-behaved by default.
- **Crawl delay** — respects per-domain rate limits, configurable per-request.
- **Render tier selection** — plain HTTP for speed, JS-rendered via Cloudflare Browser Rendering when needed, residential proxy routing as future tier.
- **SSRF protection** — blocks requests to loopback, private IP ranges, CGNAT, cloud metadata IPs, and non-HTTP(S) schemes.
- **Request validation** — enforces upper bounds on timeouts, crawl delays, TTLs, and header counts to prevent resource exhaustion.
- **Optional authentication** — bearer token support via `HERMES_API_KEY` to protect against unauthorized access.
- **Future-proof** — rate limiting, blocked domain lists, audit logging, proxy rotation, custom headers per domain — all added here once, zero changes in module code.

---

## Repository

| Property | Value |
|---|---|
| Repo name | `hermes` under `NOTTai-lab` |
| Visibility | **Public** — source code only, no CI/CD |
| Deploy repo | `hermes-deploy` under `NOTTai-lab`, **private** |
| Binary name | `hermes` |
| Internal address | `http://egress:3001` |

Source code and deployment pipeline are fully separated. The public repo contains zero references to server IPs, registry credentials, or infrastructure details.

---

## Architecture overview

```
┌─────────────────────────────────────────────────────────┐
│                    NOTT Internal Network                │
│                                                         │
│  ┌──────────────┐   ┌──────────────┐   ┌─────────────┐  │
│  │ geo-module   │   │ other-module │   │ rust-server │  │
│  └──────┬───────┘   └──────┬───────┘   └──────┬──────┘  │
│         │                  │                   │        │
│         └──────────────────┼───────────────────┘        │
│                            │ POST /fetch                │
│                            ▼                            │
│                    ┌───────────────┐                    │
│                    │    Hermes     │                    │
│                    │  :3001        │                    │
│                    └───────┬───────┘                    │
│                            │                            │
│                    ┌───────▼───────┐                    │
│                    │    Redis      │  ← response cache  │
│                    │  (shared)     │  ← robots.txt cache│
│                    └───────────────┘                    │
└─────────────────────────────────────────────────────────┘
                            │
              ┌─────────────┼──────────────┐
              │             │              │
              ▼             ▼              ▼
         Plain HTTP    CF Browser     Residential
          (reqwest)    Rendering       Proxy (v2)
```

Every request enters one handler, passes through the middleware stack, exits through one of three fetch tiers. The caller never knows or cares which tier executed.

---

## Fetch tiers

Performance and capability increase with tier number. Cost and latency also increase. Always use the lowest tier that satisfies the need.

### Tier 0 — Plain HTTP (default)
- Executed via `reqwest` with a shared connection pool
- No browser, no JavaScript execution
- Fastest: typically 50–500ms round-trip
- Works for: robots.txt, sitemaps, llm.txt, JSON APIs, RSS feeds, static HTML, most SEO metadata, response headers, any non-JS-rendered content
- Cost: zero

### Tier 1 — Cloudflare Browser Rendering (`render: true`)
- Executed via Cloudflare's `/scrape` REST API (Workers free plan)
- Spins up headless Chromium on Cloudflare's edge network
- JavaScript executes fully before content is returned
- Output formats: HTML, Markdown, JSON (structured extraction)
- Works for: SPAs, dynamically rendered pages, JS-heavy e-commerce sites
- Latency: 2–8 seconds
- Cost: free tier on Workers Free plan (limits apply)
- **Does not bypass bot protection** — Cloudflare's crawler self-identifies as a bot

### Tier 2 — Residential proxy (future, `render: "proxy"`)
- Routes through a residential proxy provider (Bright Data, Oxylabs, or self-managed)
- For heavily protected targets that block datacenter IPs
- Not implemented in v1 — field accepted and validated but returns `501 Not Implemented`
- Architecture slot reserved so callers do not need to change when implemented

---

## Request shape

All fields except `url` are optional. Hermes is designed so the simplest possible call is just a URL.

```json
{
  "url": "https://example.com/sitemap.xml",

  "method": "GET",
  "headers": {},
  "body": null,

  "render": "none",
  "render_output": "html",

  "ttl_secs": 300,
  "ignore_robots": false,
  "crawl_delay_ms": 0,
  "follow_redirects": true,
  "timeout_secs": 30
}
```

### Field reference

| Field | Type | Default | Validation | Description |
|---|---|---|---|---|
| `url` | string | **required** | `http`/`https` only; no loopback, private, or reserved IPs | Target URL |
| `method` | string | `"GET"` | | HTTP method |
| `headers` | map | `{}` | max 50 keys | Additional request headers |
| `body` | string\|null | `null` | | Request body as string |
| `render` | enum | `"none"` | | Fetch tier: `"none"`, `"browser"`, `"proxy"` |
| `render_output` | enum | `"html"` | | Output format for browser tier: `"html"`, `"markdown"`, `"json"` |
| `ttl_secs` | u64 | `300` | `0` or `1`–`86 400` (max 24h) | Cache TTL. `0` = skip cache entirely |
| `ignore_robots` | bool | `false` | | Skip robots.txt compliance check |
| `crawl_delay_ms` | u64 | `0` | `0`–`60 000` (max 1 min) | Sleep this many ms before firing request |
| `follow_redirects` | bool | `true` | | Follow HTTP redirects |
| `timeout_secs` | u64 | `30` | `1`–`120` | Request timeout in seconds |

### Render enum values

| Value | Meaning |
|---|---|
| `"none"` | Plain HTTP via reqwest (Tier 0) |
| `"browser"` | Cloudflare Browser Rendering (Tier 1) |
| `"proxy"` | Residential proxy (Tier 2, v2) |

---

## Response shape

```json
{
  "status": 200,
  "headers": {
    "content-type": "text/html; charset=utf-8"
  },
  "body": "<!DOCTYPE html>...",
  "body_encoding": "utf8",
  "render": "none",
  "cached": false,
  "fetched_at": "2026-04-17T10:00:00Z"
}
```

### Field reference

| Field | Type | Description |
|---|---|---|
| `status` | u16 | HTTP status code from upstream |
| `headers` | map | Response headers from upstream |
| `body` | string | Response body. Encoding depends on `body_encoding` |
| `body_encoding` | enum | `"utf8"` or `"base64"`. Always check before using body |
| `render` | enum | Which tier actually executed this request |
| `cached` | bool | `true` if served from Redis cache |
| `fetched_at` | string | RFC3339 UTC timestamp of the original live fetch |

`body_encoding` is `"utf8"` when the response body is valid UTF-8 text, `"base64"` for binary content (images, PDFs, archives). Caller must decode accordingly.

---

## Endpoints

```
POST /fetch          Buffered fetch. Cacheable. Full middleware stack.
POST /fetch/stream   Streaming passthrough. Never cached. Full safety pipeline.
GET  /health         Health check. Returns version. No auth required.
```

### POST /fetch

Full pipeline:
1. **Auth check** — verify `Authorization: Bearer` header if `HERMES_API_KEY` is configured.
2. **Request validation** — reject invalid URLs, SSRF targets, out-of-range field values.
3. **Cache lookup** — return immediately on hit (`cached: true`)
4. **Robots.txt check** — reject if disallowed (unless `ignore_robots: true`)
5. **Crawl delay** — sleep `crawl_delay_ms` if set
6. **Domain semaphore** — acquire per-domain concurrency permit
7. **Fetch execution** — execute via selected render tier
8. **Cache write** — store result if `ttl_secs > 0`
9. **Response** — return FetchResponse

### POST /fetch/stream

Full safety pipeline, but with zero-copy streaming:
1. **Auth check** — verify `Authorization: Bearer` header if `HERMES_API_KEY` is configured.
2. **Request validation** — reject invalid URLs, SSRF targets, out-of-range field values.
3. **Robots.txt check** — reject if disallowed (unless `ignore_robots: true`)
4. **Crawl delay** — sleep `crawl_delay_ms` if set
5. **Domain semaphore** — acquire per-domain concurrency permit
6. **Stream** — forward response bytes directly with no buffering (Tier 0 only)
7. **Headers** — forward only `content-type`, `content-encoding`, `transfer-encoding`, `cache-control`

No caching, no body size limit, no render tier support (Tier 0 only).

### GET /health

```json
{ "status": "ok", "version": "1.0.1" }
```

Always reachable (no auth required) so that load balancers and container orchestrators can probe.

---

## Crawling strategy for SEO / geo data

### What is always publicly accessible (no special handling needed)

These can all be fetched with plain `render: "none"` and will succeed on virtually every target:

| Data | How to fetch |
|---|---|
| `robots.txt` | `GET /robots.txt`, set `ignore_robots: true` |
| `sitemap.xml` | URL from robots.txt or `GET /sitemap.xml` |
| `llm.txt` | `GET /llm.txt` |
| Meta tags, OG tags | `GET /`, parse HTML response |
| Structured data (`ld+json`) | `GET /`, parse `<script type="application/ld+json">` |
| Canonical URLs | Parse `<link rel="canonical">` from HTML |
| Response headers | Included in every response — `Server`, `X-Powered-By`, tech stack signals |
| RSS/Atom feeds | URL from HTML `<link rel="alternate">` or `GET /feed` |
| DNS / WHOIS | Not HTTP — handle outside Hermes |
| SSL cert info | Connect and inspect — handle outside Hermes |

This covers 90%+ of what commercial SEO/geo tools analyze.

### When to use `render: "browser"`

Only when the target site renders its content entirely via JavaScript and plain HTML returns empty or placeholder content. Use sparingly — 5–8x slower than Tier 0. Examples: modern SPAs, some e-commerce product pages.

### Rate limiting strategy

The most common failure mode when crawling is not robots.txt blocking — it is IP bans from hitting a domain too fast. Two mechanisms prevent this:

**Per-domain concurrency** (automatic, configured globally): `DOMAIN_CONCURRENCY=3` means at most 3 simultaneous outbound requests to any single host. Protects against accidental parallel hammering.

**Crawl delay** (per-request, caller-controlled): Set `crawl_delay_ms` to match the `Crawl-delay` directive from the target's robots.txt. The geo module should read this value and pass it through.

```json
{
  "url": "https://example.com/page-2",
  "crawl_delay_ms": 1000
}
```

---

## Project structure

```
hermes/
├── src/
│   ├── main.rs           — app state, router, startup
│   ├── config.rs         — env-based config with defaults
│   ├── validate.rs       — URL validation (SSRF, scheme) + request limits
│   ├── fetch.rs          — Tier 0: plain reqwest (pre-built clients)
│   ├── render.rs         — Tier 1: Cloudflare Browser Rendering
│   ├── cache.rs          — Redis response cache (ConnectionManager)
│   ├── limiter.rs        — per-domain concurrency semaphores
│   ├── robots.rs         — robots.txt compliance (ConnectionManager)
│   ├── handlers.rs       — axum route handlers + auth + AppError
│   └── types/
│       ├── mod.rs        — re-exports only
│       ├── request.rs    — FetchRequest + RenderMode + RenderOutput
│       ├── response.rs   — FetchResponse + BodyEncoding
│       └── common.rs     — HealthResponse
├── Cargo.toml
├── Dockerfile
└── .gitignore
```

---

## Dependencies

```toml
[dependencies]
axum = { version = "0.7", features = ["macros"] }
tokio = { version = "1", features = ["full"] }
reqwest = { version = "0.12", features = ["stream", "rustls-tls"], default-features = false }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
redis = { version = "0.25", features = ["tokio-comp", "connection-manager"] }
dashmap = "5"
tokio-util = { version = "0.7", features = ["io"] }
base64 = "0.22"
sha2 = "0.10"
hex = "0.4"
texting_robots = "0.2"
url = "2"
chrono = { version = "0.4", features = ["serde"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
anyhow = "1"
subtle = "2"

[profile.release]
opt-level = 3
lto = true
codegen-units = 1
strip = true
```

**Key additions (v1.0.1):**
- `subtle` — timing-safe comparisons for auth token validation
- `connection-manager` feature in redis — persistent multiplexed Redis connections

---

## Phase 1 — Project setup

1. `cargo init .` inside `hermes/`, set package name to `hermes`
2. Add all dependencies above to `Cargo.toml`
3. Add release profile
4. Standard Rust `.gitignore` — ignore `target/`, `Cargo.lock`, `.env*`, `.idea/`

---

## Phase 2 — Types (`src/types/`)

Module folder, not a single file. Each struct in its own file. `mod.rs` re-exports everything. The rest of the codebase uses `crate::types::FetchRequest` with no knowledge of internal file split.

### `src/types/request.rs`

```rust
pub enum RenderMode {
    None,    // "none" — Tier 0, plain reqwest
    Browser, // "browser" — Tier 1, Cloudflare Browser Rendering
    Proxy,   // "proxy" — Tier 2, reserved for v2
}

pub enum RenderOutput {
    Html,     // "html"
    Markdown, // "markdown"
    Json,     // "json"
}

pub struct FetchRequest {
    pub url: String,
    pub method: String,                      // default: "GET"
    pub headers: HashMap<String, String>,    // default: empty
    pub body: Option<String>,                // default: None
    pub render: RenderMode,                  // default: None
    pub render_output: RenderOutput,         // default: Html
    pub ttl_secs: u64,                       // default: 300
    pub ignore_robots: bool,                 // default: false
    pub crawl_delay_ms: u64,                 // default: 0
    pub follow_redirects: bool,              // default: true
    pub timeout_secs: u64,                   // default: 30
}
```

Derive `Debug`, `Deserialize`. Apply `#[serde(default)]` at struct level. Implement `Default` manually — all default values live here and nowhere else. `RenderMode` and `RenderOutput` derive `Deserialize` with `#[serde(rename_all = "lowercase")]`.

### `src/types/response.rs`

```rust
pub enum BodyEncoding { Utf8, Base64 }

pub struct FetchResponse {
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: String,
    pub body_encoding: BodyEncoding,
    pub render: RenderMode,       // which tier actually ran
    pub cached: bool,
    pub fetched_at: String,       // RFC3339 UTC
}
```

Derive `Debug`, `Serialize`, `Deserialize` on both. `BodyEncoding` and `RenderMode` serialize as lowercase strings.

### `src/types/common.rs`

```rust
pub struct HealthResponse { pub status: &'static str, pub version: &'static str }
```

Derive `Debug`, `Serialize`.

### `src/types/mod.rs`

Declare all submodules, re-export everything publicly. No logic.

---

## Phase 3 — Validation (`src/validate.rs`)

**New in v1.0.1** — centralized input validation for security.

### URL validation

- `validate_url(url_str: &str) -> Result<(), String>` — rejects:
  - Non-`http`/`https` schemes (no `file://`, `ftp://`, etc.)
  - IP literals in loopback (`127.x`, `::1`), private (`10/8`, `172.16-31/12`, `192.168/16`), link-local (`169.254/16`), CGNAT (`100.64/10`), or documentation ranges
  - Hostnames `localhost`, `ip6-localhost`, `ip6-loopback`

### Request validation

- `validate_request(req: &FetchRequest) -> Result<(), String>` — enforces:
  - `timeout_secs` in range `1`–`120`
  - `crawl_delay_ms` ≤ `60 000` (1 minute)
  - `ttl_secs` ≤ `86 400` (24 hours), or `0` (no cache)
  - `headers` ≤ `50` keys

Called from `handlers.rs` at the start of both `/fetch` and `/fetch/stream` endpoints.

---

## Phase 4 — Config (`src/config.rs`)

**Updated in v1.0.1** — added `api_key` field.

```rust
pub struct Config {
    pub port: u16,                    // PORT, default 3001
    pub redis_url: String,            // REDIS_URL, default redis://127.0.0.1:6379
    pub user_agent: String,           // USER_AGENT, default HermesBot/1.0
    pub domain_concurrency: usize,    // DOMAIN_CONCURRENCY, default 3
    pub default_ttl_secs: u64,        // DEFAULT_TTL_SECS, default 300
    pub max_body_bytes: usize,        // MAX_BODY_BYTES, default 10485760 (10MB)
    pub cf_api_token: Option<String>, // CF_API_TOKEN, default None (Tier 1 disabled if absent)
    pub cf_account_id: Option<String>,// CF_ACCOUNT_ID, default None
    pub api_key: Option<String>,      // HERMES_API_KEY, default None (auth disabled if absent)
}
```

Implement `Default` with all default values. Implement `Config::from_env()` using:

```rust
fn get_env_parse<T>(key: &str, default: T) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Debug,
{
    match env::var(key) {
        Ok(val) => val.parse::<T>().unwrap_or_else(|_| {
            panic!("Config error: '{}' has invalid value '{}'", key, val)
        }),
        Err(_) => default,
    }
}
```

`from_env()` builds `Self::default()` then overrides each field. Panics on invalid values — process must not start with bad config.

`api_key` is `Option<String>` — if present and non-empty, requests to `/fetch` and `/fetch/stream` require `Authorization: Bearer <api_key>`. If absent, endpoints are open.

---

## Phase 5 — Core modules

### `src/cache.rs`

Responsibility: Redis-backed response cache with multiplexed connection.

**Updated in v1.0.1:**
- Uses `redis::aio::ConnectionManager` instead of creating a new multiplexed connection per operation
- `cache_key()` signature updated: includes `render: &str` parameter; hashes `method + url + render + sorted(headers)`
- Cache key prefix bumped `v1 → v2` to invalidate old entries that don't include render mode

Implementation:
- `Cache::new(redis_url: &str) -> Result<Self>` — async, builds `ConnectionManager`, returns error on connection failure
- `Cache::cache_key(method, url, headers, render) -> String` — SHA256 of all inputs, hex encoded, prefixed `hermes:v2:`
- `cache.get(key) -> Option<FetchResponse>` — async, clones `ConnectionManager`, returns `None` on any error (Redis down = cache miss, not crash)
- `cache.set(key, response, ttl_secs)` — async, skip if `ttl_secs == 0`, log warning on error, never propagate

### `src/limiter.rs`

Responsibility: prevent hammering any single host with too many parallel requests.

- `DomainLimiter::new(max_per_domain: usize) -> Self`
- `limiter.acquire(url: &str) -> OwnedSemaphorePermit` — async, extract hostname via `url` crate, lazy `DashMap<String, Arc<Semaphore>>` insert on first use per host, acquire and return. Caller holds permit for the duration of the outbound request, drop releases the slot.

### `src/robots.rs`

Responsibility: answer one question — "are we allowed to fetch this URL?" Compliance gate only.

**Updated in v1.0.1:**
- Uses `redis::aio::ConnectionManager` instead of creating a new multiplexed connection per operation
- Fixed robots URL construction: now uses `url.origin().ascii_serialization()` to include port, so `http://example.com:8080` fetches from `http://example.com:8080/robots.txt` (not `http://example.com/robots.txt`)

Implementation:
- `RobotsChecker::new(redis_url, http: reqwest::Client, user_agent: String) -> Result<Self>` — async
- `checker.is_allowed(url: &str) -> bool` — async, **fails open** (returns `true`) on any internal error. Log warning when failing open.
- Internal flow:
  1. Parse `{scheme}://{host}:{port}` from target URL
  2. Build `{scheme}://{host}:{port}/robots.txt` via `url.origin()`
  3. Redis key: `hermes:robots:v1:{origin}` (includes port)
  4. Check Redis — use cached content if present
  5. If not cached: fetch with 5s hard timeout. On 404, non-2xx, or network error — treat as no restrictions, cache empty string
  6. Parse with `texting_robots::Robot::new(&user_agent, content.as_bytes())`
  7. Return `robot.allowed(url)`
- Cache TTL: 86400 seconds (24h)
- `http` client passed in from outside — do not construct a new one here

### `src/fetch.rs`

Responsibility: Tier 0 execution. Plain HTTP, nothing else.

**Updated in v1.0.1:**
- Pre-builds a `no_redirect_client` at construction time
- `execute()` selects between `client` and `no_redirect_client` instead of building a new client per request

Implementation:
- `Fetcher::new(user_agent: String, max_body_bytes: usize) -> Self` — builds two `reqwest::Client` instances at construction:
  - `client` with default redirect policy
  - `no_redirect_client` with `redirect::Policy::none()`
  - Both configured: `pool_max_idle_per_host(10)`, TCP keepalive 60s, user agent set
- `fetcher.execute(req: &FetchRequest) -> Result<FetchResponse>`:
  - Select client based on `follow_redirects` field (no new client build)
  - Build and fire request with method, headers, body, timeout
  - Read body up to `max_body_bytes` — return `Err` if exceeded
  - UTF-8 decode attempt → `BodyEncoding::Utf8`, failure → base64 → `BodyEncoding::Base64`
  - `fetched_at` = current UTC RFC3339
  - `render` field in response = `RenderMode::None`

### `src/render.rs`

Responsibility: Tier 1 execution via Cloudflare Browser Rendering REST API.

- `BrowserRenderer::new(api_token: String, account_id: String) -> Self`
- `renderer.execute(req: &FetchRequest) -> Result<FetchResponse>`:
  - Call `POST https://api.cloudflare.com/client/v4/accounts/{account_id}/browser-rendering/scrape`
  - Request body:
    ```json
    {
      "url": "...",
      "rejectResourceTypes": ["image", "media", "font"],
      "gotoOptions": { "waitUntil": "networkidle0", "timeout": 30000 }
    }
    ```
  - `rejectResourceTypes` blocks images/media/fonts — faster page load, not needed for SEO data
  - Auth header: `Authorization: Bearer {api_token}`
  - Parse response, extract content based on `render_output` field
  - Return as `FetchResponse` with `render: RenderMode::Browser`
  - If `cf_api_token` or `cf_account_id` missing — return `Err` with message "Browser rendering not configured"

---

## Phase 6 — Handlers (`src/handlers.rs`)

**Updated in v1.0.1:**
- Auth validation via constant-time token comparison
- Request validation (URL + field caps)
- Updated `/fetch/stream` to include full safety pipeline (auth, validate, robots, delay)
- Render mode included in cache key hash

### App state

```rust
pub struct AppState {
    pub cache:    Arc<Cache>,
    pub fetcher:  Arc<Fetcher>,
    pub renderer: Option<Arc<BrowserRenderer>>,
    pub limiter:  Arc<DomainLimiter>,
    pub robots:   Arc<RobotsChecker>,
    pub api_key:  Option<String>,  // NEW: optional bearer token
}
```

### Auth helper

```rust
fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let Some(ref expected) = state.api_key else {
        return Ok(());
    };

    let provided = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    // Constant-time comparison via subtle::ConstantTimeEq
    match provided {
        Some(key) if bool::from(key.as_bytes().ct_eq(expected.as_bytes())) => Ok(()),
        _ => Err(AppError::Unauthorized),
    }
}
```

### `GET /health`

Returns `HealthResponse { status: "ok", version: env!("CARGO_PKG_VERSION") }`. No state access, no auth required.

### `POST /fetch`

```
1. check_auth(state, headers)                              [NEW]
2. validate_request(&req)                                 [NEW]
3. Compute cache key: cache_key(method, url, headers, render)  [UPDATED: includes render]
4. If ttl_secs > 0 → check cache → return on hit (cached: true)
5. If !ignore_robots → robots.is_allowed(url) → 403 on false
6. If crawl_delay_ms > 0 → tokio::time::sleep(Duration::from_millis(crawl_delay_ms))
7. Acquire domain permit via limiter.acquire(url)
8. Match render tier:
   RenderMode::None    → fetcher.execute(req)
   RenderMode::Browser → renderer (if Some) or 501
   RenderMode::Proxy   → 501 (not implemented)
9. If ttl_secs > 0 → cache.set (log warning on failure, never error)
10. Return response
```

### `POST /fetch/stream`

**Updated in v1.0.1:** Now enforces the full safety pipeline.

```
1. check_auth(state, headers)                              [NEW]
2. validate_request(&req)                                 [NEW]
3. If !ignore_robots → robots.is_allowed(url) → 403 on false  [NEW]
4. If crawl_delay_ms > 0 → tokio::time::sleep(Duration::from_millis(crawl_delay_ms))  [NEW]
5. Acquire domain permit via limiter.acquire(url)
6. Build and send via fetcher.client directly (Tier 0 only)
7. Forward status code
8. Forward headers: content-type, content-encoding, transfer-encoding, cache-control
9. Body::from_stream(response.bytes_stream()) — zero buffering
```

### `AppError`

**Updated in v1.0.1:** Added `Unauthorized` variant.

```rust
pub enum AppError {
    FetchFailed(String),    // 502 Bad Gateway
    Forbidden(String),      // 403 Forbidden (robots.txt)
    BadRequest(String),     // 400 Bad Request (validation)
    NotImplemented(String), // 501 Not Implemented
    BodyTooLarge(String),   // 502 Bad Gateway
    Unauthorized,           // 401 Unauthorized [NEW]
}
```

---

## Phase 7 — Main (`src/main.rs`)

**Updated in v1.0.1:**
- Async init for Cache and RobotsChecker (both now call `ConnectionManager::new().await`)
- Added `mod validate`
- Pass `api_key` to AppState

Implementation:
- Declare all modules
- Initialize `tracing_subscriber` with `EnvFilter`, fallback `"info"`
- Load `Config::from_env()`
- Build `Fetcher` — its `reqwest::Client` is also cloned into `RobotsChecker`
- Build `renderer: Option<Arc<BrowserRenderer>>` — `Some` only if both CF env vars present
- Construct `AppState` including `api_key` from config
- Build router:
  ```
  POST /fetch         → handlers::fetch
  POST /fetch/stream  → handlers::fetch_stream
  GET  /health        → handlers::health
  ```
- Bind `TcpListener` on `0.0.0.0:{port}`, serve
- Log startup summary: port, redis url, CF rendering enabled/disabled, domain concurrency, auth enabled/disabled

---

## Phase 8 — Docker

Multi-stage `Dockerfile`:

**Stage 1 — `builder`** (`rust:1.82-slim`):
- Install `pkg-config libssl-dev`
- Copy `Cargo.toml`, stub `src/main.rs`, `cargo build --release` — caches deps layer
- Copy real `src/`, touch `src/main.rs`, `cargo build --release`

**Stage 2 — runtime** (`debian:bookworm-slim`):
- Install `ca-certificates` only
- Copy binary to `/usr/local/bin/hermes`
- Set all env var defaults via `ENV`
- `EXPOSE 3001`
- `HEALTHCHECK --interval=15s --timeout=5s`: `curl -sf http://localhost:3001/health || exit 1`
- `CMD ["hermes"]`

---

## Phase 9 — Deployment

### Docker Compose addition (`/opt/nott/docker-compose.yml`)

```yaml
egress:
  image: <your-registry>/nott/hermes:latest
  restart: unless-stopped
  environment:
    REDIS_URL: redis://redis:6379
    PORT: "3001"
    USER_AGENT: "HermesBot/1.0"
    DOMAIN_CONCURRENCY: "3"
    DEFAULT_TTL_SECS: "300"
    MAX_BODY_BYTES: "10485760"
    HERMES_API_KEY: ""             # ← inject as secret
    # CF_API_TOKEN: ""             # ← inject as secret to enable browser rendering
    # CF_ACCOUNT_ID: ""            # ← inject as secret to enable browser rendering
  depends_on:
    - redis
  networks:
    - nott-internal
```

Not exposed via nginx. Internal only.

### `hermes-deploy` CI/CD (`.github/workflows/deploy.yml`)

- Trigger: `repository_dispatch` from `hermes` repo on push to `main`
- Steps: checkout `hermes`, configure insecure registry daemon, docker login, build + push `:latest` and `:{sha8}`, SSH to Hetzner server, `docker pull`, `docker compose up -d --no-deps --force-recreate egress`
- Secrets: `REGISTRY_HOST`, `REGISTRY_USER`, `REGISTRY_PASSWORD`, `SSH_HOST`, `SSH_USER`, `SSH_PRIVATE_KEY`

---

## Request examples

**Minimal:**
```json
{ "url": "https://example.com" }
```

**Fetch robots.txt (skip circular robots check):**
```json
{ "url": "https://example.com/robots.txt", "ignore_robots": true, "ttl_secs": 86400 }
```

**Fetch sitemap:**
```json
{ "url": "https://example.com/sitemap.xml", "ttl_secs": 3600 }
```

**API call with auth, no cache:**
```json
{
  "url": "https://api.example.com/v1/data",
  "headers": { "Authorization": "Bearer upstream-token" },
  "ttl_secs": 0
}
```

**POST request, never cache:**
```json
{
  "url": "https://api.example.com/v1/orders",
  "method": "POST",
  "headers": { "Content-Type": "application/json" },
  "body": "{\"product_id\": 42}",
  "ttl_secs": 0
}
```

**JS-rendered page:**
```json
{
  "url": "https://example.com",
  "render": "browser",
  "render_output": "markdown",
  "ttl_secs": 600
}
```

**Geo/SEO crawl with rate limiting:**
```json
{
  "url": "https://competitor.com/page",
  "crawl_delay_ms": 1000,
  "ttl_secs": 3600
}
```

**Binary file:**
```json
{ "url": "https://example.com/file.pdf", "ttl_secs": 86400 }
```
Response arrives with `"body_encoding": "base64"` — decode before use.

**Streaming (SSE or large download):**
```json
POST /fetch/stream
{ "url": "https://api.example.com/events" }
```
Response is raw bytes, forwarded directly. Never cached. Full safety pipeline applied.

**Don't follow redirects:**
```json
{ "url": "https://example.com/old", "follow_redirects": false, "ttl_secs": 0 }
```

---

## Performance characteristics

| Scenario | Expected latency |
|---|---|
| Cache hit (Redis) | < 5ms |
| Tier 0, fast target | 50–300ms |
| Tier 0, slow target | 300ms–3s |
| Tier 1, browser render | 2–8s |
| Robots.txt check (cached) | < 5ms (Redis) |
| Robots.txt check (cold) | +50–300ms one-time per domain per 24h |

The dominant cost for most module use cases is the network round-trip to the target. Everything else — Redis, semaphore, hashing — is microseconds.

**v1.0.1 optimizations:**
- Redis `ConnectionManager` eliminates per-operation connection overhead (one persistent multiplexed connection)
- Pre-built `no_redirect_client` in Fetcher avoids per-request client allocation
- Cache key v2 includes render mode to prevent render-mode collisions

---

## Error responses

All errors return JSON:
```json
{ "error": "human readable message", "code": "MACHINE_READABLE_CODE" }
```

| Code | HTTP | Meaning |
|---|---|---|
| `FETCH_FAILED` | 502 | Upstream request failed (timeout, DNS, connection refused) |
| `FORBIDDEN` | 403 | Blocked by robots.txt |
| `BAD_REQUEST` | 400 | Invalid URL, SSRF target, out-of-range field value |
| `NOT_IMPLEMENTED` | 501 | Requested render tier not available |
| `BODY_TOO_LARGE` | 502 | Response exceeded `MAX_BODY_BYTES` |
| `UNAUTHORIZED` | 401 | Missing or invalid `Authorization: Bearer` token |
