//! Hermes — unified HTTP egress gateway.
//!
//! Entry point for the Hermes binary. Responsibilities:
//! 1. Initialise structured logging via `tracing`.
//! 2. Load [`Config`] from environment variables.
//! 3. Construct all shared components ([`Cache`], [`Fetcher`],
//!    [`BrowserRenderer`], [`DomainLimiter`], [`RobotsChecker`]).
//! 4. Build the Axum router and start the TCP listener.

use axum::{Router, routing};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

mod cache;
mod config;
mod fetch;
mod handlers;
mod limiter;
mod render;
mod robots;
mod types;
mod validate;

use cache::Cache;
use config::Config;
use fetch::Fetcher;
use handlers::AppState;
use limiter::DomainLimiter;
use render::BrowserRenderer;
use robots::RobotsChecker;

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Config::from_env();

    let fetcher = Arc::new(Fetcher::new(
        config.user_agent.clone(),
        config.max_body_bytes,
    ));

    let renderer = match (&config.cf_api_token, &config.cf_account_id) {
        (Some(token), Some(account_id)) => {
            tracing::info!("browser rendering enabled (Cloudflare)");
            Some(Arc::new(BrowserRenderer::new(
                token.clone(),
                account_id.clone(),
            )))
        }
        _ => {
            tracing::info!("browser rendering disabled (CF_API_TOKEN or CF_ACCOUNT_ID not set)");
            None
        }
    };

    let state = AppState {
        cache: Arc::new(
            Cache::new(&config.redis_url)
                .await
                .expect("failed to initialize cache"),
        ),
        fetcher: fetcher.clone(),
        renderer,
        limiter: Arc::new(DomainLimiter::new(config.domain_concurrency)),
        robots: Arc::new(
            RobotsChecker::new(
                &config.redis_url,
                fetcher.client.clone(),
                config.user_agent.clone(),
            )
            .await
            .expect("failed to initialize robots checker"),
        ),
        api_key: config.api_key.clone(),
    };

    let app = Router::new()
        .route("/fetch", routing::post(handlers::fetch))
        .route("/fetch/stream", routing::post(handlers::fetch_stream))
        .route("/health", routing::get(handlers::health))
        .with_state(state.clone());

    let addr = format!("0.0.0.0:{}", config.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|_| panic!("failed to bind to {addr}"));

    tracing::info!(
        port = config.port,
        redis = %config.redis_url,
        user_agent = %config.user_agent,
        domain_concurrency = config.domain_concurrency,
        browser_rendering = state.renderer.is_some(),
        auth = state.api_key.is_some(),
        "hermes started"
    );

    axum::serve(listener, app).await.unwrap();
}
