FROM rust:1.82-slim AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependency compilation layer separately
COPY Cargo.toml ./
RUN mkdir src && echo "fn main(){}" > src/main.rs
RUN cargo build --release
RUN rm -rf src

# Build real binary
COPY src ./src
RUN touch src/main.rs
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/hermes /usr/local/bin/hermes

ENV PORT=3001 \
    REDIS_URL=redis://127.0.0.1:6379 \
    USER_AGENT=HermesBot/1.0 \
    DOMAIN_CONCURRENCY=3 \
    DEFAULT_TTL_SECS=300 \
    MAX_BODY_BYTES=10485760

EXPOSE 3001

HEALTHCHECK --interval=15s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -sf http://localhost:3001/health || exit 1

CMD ["hermes"]
