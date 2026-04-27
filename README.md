# Hermes — Unified HTTP Egress Gateway

Hermes is a standalone Rust microservice that acts as the **single outbound HTTP gateway** for the NOTT platform. Every module, service, or component that needs to fetch anything from the external web goes through Hermes.

## Features

- **Single outbound identity** — one IP, one User-Agent visible to the external web. All crawling activity looks like one consistent, well-behaved bot.
- **Response caching** — Redis-backed cache with configurable TTL. Identical requests within the TTL window hit Redis instead of the network.
- **Per-domain concurrency control** — prevents accidentally hammering a single host with parallel requests, which would cause IP bans.
- **Robots.txt compliance** — automatically checked before every fetch (configurable per-request). Keeps the crawler well-behaved by default.
- **Crawl delay support** — respects per-domain rate limits, configurable per-request.
- **Multiple render tiers** — plain HTTP for speed, JS-rendered via Cloudflare Browser Rendering when needed, residential proxy reserved for v2.
- **SSRF protection** — rejects requests targeting loopback, private, link-local, CGNAT, and cloud-metadata IP ranges. Only `http`/`https` schemes are allowed.
- **Request validation** — enforces upper bounds on timeout, crawl delay, TTL, and header count to prevent resource exhaustion.
- **Optional bearer-token auth** — set `HERMES_API_KEY` to require `Authorization: Bearer <key>` on all non-health endpoints.
- **Future-proof architecture** — rate limiting, blocked domain lists, audit logging, proxy rotation, custom headers — all added here once, zero changes in module code.

## Architecture

```
┌─────────────────────────────────────────────┐
│         NOTT Internal Network               │
│                                             │
│  Module A    Module B    Module C           │
│      │           │           │              │
│      └───────────┼───────────┘              │
│                  │ POST /fetch              │
│                  ▼                          │
│          ┌───────────────┐                  │
│          │    Hermes     │                  │
│          │   :3001       │                  │
│          └───────┬───────┘                  │
│                  │                          │
│          ┌───────▼───────┐                  │
│          │     Redis     │                  │
│          │    Cache      │                  │
│          └───────────────┘                  │
└─────────────────────────────────────────────┘
              │
    ┌─────────┼──────────┐
    │         │          │
    ▼         ▼          ▼
 Plain HTTP  CF Browser  Residential
  (Tier 0)   (Tier 1)    (Tier 2, v2)
```

### Fetch Tiers

**Tier 0 — Plain HTTP (default)**
- Executed via `reqwest` with a shared connection pool
- No browser, no JavaScript execution
- Fastest: typically 50–500ms round-trip
- Works for: robots.txt, sitemaps, llm.txt, JSON APIs, RSS feeds, static HTML, meta tags, response headers
- Cost: zero

**Tier 1 — Cloudflare Browser Rendering**
- Executed via Cloudflare's `/scrape` REST API
- Spins up headless Chromium on Cloudflare's edge network
- JavaScript executes fully before content is returned
- Output formats: HTML, Markdown, JSON (v2)
- Works for: SPAs, dynamically rendered pages, JS-heavy e-commerce sites
- Latency: 2–8 seconds
- Cost: free tier on Workers Free plan (50k req/month), or $0.50 per million requests

**Tier 2 — Residential Proxy (v2)**
- Routes through a residential proxy provider
- For heavily protected targets that block datacenter IPs
- Not implemented in v1 — returns `501 Not Implemented`

## Quick Start

### Prerequisites

- Rust 1.82 or later
- Redis 6.0 or later (for caching and robots.txt storage)
- Docker (optional, for containerized deployment)

### Local Development

```bash
# Clone the repository
git clone https://github.com/NOTTai-lab/hermes.git
cd hermes

# Set environment variables (optional, uses defaults)
export PORT=3001
export REDIS_URL=redis://127.0.0.1:6379
export USER_AGENT=HermesBot/1.0
export DOMAIN_CONCURRENCY=3
export MAX_BODY_BYTES=10485760

# Build and run
cargo build --release
./target/release/hermes
```

### With Docker Compose

```bash
docker-compose up -d
```

Check if running:
```bash
curl http://localhost:3001/health
```

## API

### `GET /health`

Health check endpoint. Authentication is **not** required — this endpoint is always reachable so that load balancers and container orchestrators can probe without credentials.

**Request:**
```bash
curl http://localhost:3001/health
```

**Response:**
```json
{
  "status": "ok",
  "version": "1.0.1"
}
```

### `POST /fetch`

Fetch a URL through the full Hermes pipeline:

1. **Auth check** — reject if `HERMES_API_KEY` is set and the token is missing/wrong.
2. **Request validation** — reject invalid URLs, SSRF targets, and out-of-range field values.
3. **Cache lookup** — return cached response immediately if available and `ttl_secs > 0`.
4. **Robots.txt check** — reject if disallowed (unless `ignore_robots: true`).
5. **Crawl delay** — sleep `crawl_delay_ms` milliseconds.
6. **Domain semaphore** — acquire a per-hostname concurrency permit.
7. **Fetch** — execute via the selected render tier.
8. **Cache write** — store result if `ttl_secs > 0`.

**Request:**
```bash
curl -X POST http://localhost:3001/fetch \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $HERMES_API_KEY" \
  -d '{
    "url": "https://example.com/robots.txt",
    "method": "GET",
    "ignore_robots": true,
    "ttl_secs": 86400
  }'
```

**Response:**
```json
{
  "status": 200,
  "headers": {
    "content-type": "text/plain; charset=utf-8",
    "content-length": "1234"
  },
  "body": "User-agent: *\nDisallow: /admin",
  "body_encoding": "utf8",
  "render": "none",
  "cached": false,
  "fetched_at": "2026-04-26T10:00:00Z"
}
```

### `POST /fetch/stream`

Stream a response directly without buffering or caching. For SSE, large downloads, or streaming responses.

The same safety pipeline (auth, URL validation, robots.txt, crawl delay, domain semaphore) is applied — streaming requests are never exempt from access controls.

**Request:**
```bash
curl -X POST http://localhost:3001/fetch/stream \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $HERMES_API_KEY" \
  -d '{"url": "https://api.example.com/events"}'
```

**Response:** Raw bytes streamed directly from upstream. `content-type`, `content-encoding`, `transfer-encoding`, and `cache-control` headers are forwarded.

## Request Fields

| Field | Type | Default | Limits | Description |
|---|---|---|---|---|
| `url` | string | **required** | http/https only, no private IPs | Target URL |
| `method` | string | `"GET"` | | HTTP method |
| `headers` | object | `{}` | max 50 keys | Additional request headers |
| `body` | string | `null` | | Request body as string |
| `render` | enum | `"none"` | | Fetch tier: `"none"`, `"browser"`, `"proxy"` |
| `render_output` | enum | `"html"` | | Output format for browser tier: `"html"`, `"markdown"`, `"json"` |
| `ttl_secs` | u64 | `300` | max 86 400 (24 h) | Cache TTL. `0` = skip cache |
| `ignore_robots` | bool | `false` | | Skip robots.txt compliance check |
| `crawl_delay_ms` | u64 | `0` | max 60 000 (1 min) | Sleep this many ms before firing request |
| `follow_redirects` | bool | `true` | | Follow HTTP redirects |
| `timeout_secs` | u64 | `30` | 1–120 | Request timeout in seconds |

## Response Fields

| Field | Type | Description |
|---|---|---|
| `status` | u16 | HTTP status code from upstream |
| `headers` | object | Response headers from upstream |
| `body` | string | Response body. Encoding depends on `body_encoding` |
| `body_encoding` | enum | `"utf8"` or `"base64"`. Check before using body |
| `render` | enum | Which tier actually executed this request |
| `cached` | bool | `true` if served from Redis cache |
| `fetched_at` | string | RFC3339 UTC timestamp of the original live fetch |

## Configuration

All configuration via environment variables. Sensible defaults provided.

```bash
# Server
PORT=3001                          # Listen port
REDIS_URL=redis://127.0.0.1:6379  # Redis connection URL

# Behavior
USER_AGENT=HermesBot/1.0           # User-Agent header for all requests
DOMAIN_CONCURRENCY=3               # Max concurrent requests per domain
DEFAULT_TTL_SECS=300               # Default cache TTL
MAX_BODY_BYTES=10485760            # Max response body size (10 MiB)

# Security (recommended in production)
HERMES_API_KEY=                    # Bearer token required on /fetch and /fetch/stream.
                                   # Leave unset to disable auth (trusted-network only).
                                   # Always pair with TLS so the key is not sent in clear text.

# Cloudflare Browser Rendering (optional, Tier 1)
CF_API_TOKEN=                      # Cloudflare API token
CF_ACCOUNT_ID=                     # Cloudflare account ID

# Logging
RUST_LOG=info                      # Tracing log level (debug, info, warn, error)
```

If `CF_API_TOKEN` or `CF_ACCOUNT_ID` are missing, Tier 1 is disabled and requests with `render: "browser"` return `501 Not Implemented`.

## Examples

### Fetch a robots.txt file

```bash
curl -X POST http://localhost:3001/fetch \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $HERMES_API_KEY" \
  -d '{
    "url": "https://example.com/robots.txt",
    "ignore_robots": true,
    "ttl_secs": 86400
  }'
```

### Fetch a sitemap

```bash
curl -X POST http://localhost:3001/fetch \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $HERMES_API_KEY" \
  -d '{
    "url": "https://example.com/sitemap.xml",
    "ttl_secs": 3600
  }'
```

### Fetch with upstream API authentication

```bash
curl -X POST http://localhost:3001/fetch \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $HERMES_API_KEY" \
  -d '{
    "url": "https://api.example.com/v1/data",
    "headers": {
      "Authorization": "Bearer upstream-token"
    },
    "ttl_secs": 0
  }'
```

### Fetch a JS-rendered page

```bash
curl -X POST http://localhost:3001/fetch \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $HERMES_API_KEY" \
  -d '{
    "url": "https://spa.example.com",
    "render": "browser",
    "render_output": "markdown",
    "ttl_secs": 600
  }'
```

### Stream SSE events

```bash
curl -X POST http://localhost:3001/fetch/stream \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $HERMES_API_KEY" \
  -d '{"url": "https://api.example.com/events"}'
```

### Polite crawl with delay

```bash
# First, fetch and parse robots.txt for the Crawl-delay directive
curl -X POST http://localhost:3001/fetch \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $HERMES_API_KEY" \
  -d '{
    "url": "https://example.com/robots.txt",
    "ignore_robots": true
  }'

# Then pass the crawl delay on subsequent requests
curl -X POST http://localhost:3001/fetch \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $HERMES_API_KEY" \
  -d '{
    "url": "https://example.com/page-2",
    "crawl_delay_ms": 1000,
    "ttl_secs": 3600
  }'
```

## Error Responses

All errors return JSON with a machine-readable `code` and a human-readable `error` message.

```json
{
  "error": "robots.txt disallows crawling https://example.com/admin",
  "code": "FORBIDDEN"
}
```

| Code | HTTP | Meaning |
|---|---|---|
| `FETCH_FAILED` | 502 | Upstream request failed (timeout, DNS, connection refused) |
| `FORBIDDEN` | 403 | Blocked by robots.txt |
| `BAD_REQUEST` | 400 | Invalid URL, SSRF target, or out-of-range field value |
| `NOT_IMPLEMENTED` | 501 | Requested render tier not available |
| `BODY_TOO_LARGE` | 502 | Response exceeded `MAX_BODY_BYTES` |
| `UNAUTHORIZED` | 401 | Missing or invalid `Authorization: Bearer` token |

## Performance Characteristics

| Scenario | Expected latency |
|---|---|
| Cache hit | < 5ms |
| Tier 0, fast target | 50–300ms |
| Tier 0, slow target | 300ms–3s |
| Tier 1, browser render | 2–8s |
| Robots.txt check (cached) | < 5ms |
| Robots.txt check (cold) | +50–300ms one-time per domain per 24h |

The dominant cost for most use cases is the network round-trip to the target. Everything else — Redis, semaphore, hashing — is microseconds.

Redis operations use a persistent `ConnectionManager` (one multiplexed connection per `Cache`/`RobotsChecker` instance), so there is no per-request connection overhead.

## Project Structure

```
hermes/
├── src/
│   ├── main.rs           # App entry point, router, startup
│   ├── config.rs         # Environment-based config
│   ├── validate.rs       # URL validation (SSRF, scheme) + request field limits
│   ├── cache.rs          # Redis response cache (ConnectionManager)
│   ├── limiter.rs        # Per-domain concurrency control
│   ├── robots.rs         # Robots.txt compliance (ConnectionManager)
│   ├── fetch.rs          # Tier 0: plain HTTP
│   ├── render.rs         # Tier 1: Cloudflare Browser Rendering
│   ├── handlers.rs       # Axum route handlers + AppError
│   └── types/
│       ├── mod.rs        # Type re-exports
│       ├── request.rs    # FetchRequest + RenderMode
│       ├── response.rs   # FetchResponse + BodyEncoding
│       └── common.rs     # HealthResponse
├── Cargo.toml
├── Dockerfile            # Multi-stage build
└── .github/
    └── workflows/
        ├── ci.yml        # Test, lint, format
        └── deploy.yml    # Build, push, SSH deploy (Hetzner)
```

## Development

### Run tests

```bash
cargo test --release
```

### Run clippy linter

```bash
cargo clippy --release -- -D warnings
```

### Check formatting

```bash
cargo fmt -- --check
```

### Build release binary

```bash
cargo build --release
./target/release/hermes
```

## Deployment

### Docker

Build and run:
```bash
docker build -t hermes:latest .
docker run -d \
  -e REDIS_URL=redis://redis:6379 \
  -e HERMES_API_KEY=your-secret-key \
  -p 3001:3001 \
  hermes:latest
```

### Docker Compose

Add to your `docker-compose.yml`:

```yaml
services:
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
      HERMES_API_KEY: ""        # ← inject as secret
      # CF_API_TOKEN: ""        # ← inject as secret to enable browser rendering
      # CF_ACCOUNT_ID: ""       # ← inject as secret to enable browser rendering
    depends_on:
      - redis
    networks:
      - nott-internal
```

## CI/CD

### GitHub Actions

**CI** (`.github/workflows/ci.yml`):
- Runs on PR and push to main
- Tests with `cargo test --release`
- Linting with `cargo clippy`
- Format checking with `cargo fmt`
- Caches dependencies for speed

**CD** (`.github/workflows/deploy.yml`):
- Triggered by `repository_dispatch` from the public `hermes` repo
- Builds Docker image with build cache
- Pushes to private registry on Hetzner
- SSHes into production server and restarts container via Docker Compose
- Verifies deployment with a health check

## Monitoring

### Health Check

```bash
curl http://localhost:3001/health
```

### Logs

```bash
# View logs from Docker container
docker logs hermes

# With follow
docker logs -f hermes

# Filter by level
RUST_LOG=debug docker run hermes
```

### Key metrics to watch

- Request latency (cache hits vs. Tier 0 vs. Tier 1)
- Cache hit ratio
- Robots.txt block rate
- Error rates by code (`FETCH_FAILED`, `FORBIDDEN`, `UNAUTHORIZED`, `BODY_TOO_LARGE`)
- Per-domain concurrency (check limiter is working)

## Troubleshooting

### "failed to initialize cache" / "failed to initialize robots checker"

Check Redis is running:
```bash
redis-cli ping
```

Verify `REDIS_URL` environment variable:
```bash
echo $REDIS_URL
```

### "browser rendering is not configured"

Set `CF_API_TOKEN` and `CF_ACCOUNT_ID`:
```bash
export CF_API_TOKEN=your-token
export CF_ACCOUNT_ID=your-account-id
```

### "robots.txt disallows crawling"

The URL is blocked by the site's robots.txt. Either:
1. Wait 24 hours for the cached robots.txt to expire
2. Manually verify the block is accurate
3. Set `ignore_robots: true` if you have permission to crawl

### "response body exceeds limit"

The response is larger than `MAX_BODY_BYTES` (default 10 MiB). Either:
1. Increase `MAX_BODY_BYTES` in config
2. Use `/fetch/stream` for large file downloads instead

### "valid Bearer token required" (401)

Set the `Authorization: Bearer <key>` header to match the `HERMES_API_KEY` configured on the server.

### "scheme 'X' not allowed" or "target IP X is not routable" (400)

The target URL failed SSRF validation. Only `http`/`https` URLs pointing to public IP addresses are accepted.

### Slow responses

1. Check if it's a cache hit (look for `"cached": true` in response)
2. Check target website latency — Hermes adds microseconds, not seconds
3. For Tier 1 (browser rendering), latency of 2–8s is expected

## License

Proprietary — NOTT platform internal use only.

## Support

For issues or questions, open an issue in the `hermes` repository or contact the NOTT team.

---

**Repository:**
- Public: https://github.com/NOTTai-lab/hermes
- Private (deploy): https://github.com/NOTTai-lab/hermes-deploy

**Internal address:** `http://egress:3001`
