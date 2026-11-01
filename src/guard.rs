//! Target guards that run while a request is in flight.
//!
//! [`crate::validate::validate_url`] only sees the URL the caller sent. Two
//! paths get around it:
//!
//! - a hostname that resolves to a private address (an attacker-registered
//!   domain pointing at `10.0.0.5`, or DNS rebinding between validation and
//!   connect);
//! - a redirect whose `Location` points somewhere `validate_url` would refuse.
//!
//! [`PublicOnlyResolver`] closes the first: reqwest connects only to the
//! addresses it returns, so filtering here covers every hop and every retry.
//! [`redirect_policy`] closes the second for IP literals and schemes, which
//! never reach the resolver.
//!
//! [`describe_fetch_error`] turns a reqwest error into a message callers can
//! act on (`request failed [tls_certificate]: …`) without the URL, which may
//! carry a query string the control plane deliberately keeps out of its logs.

use crate::validate::{is_blocked_ip, validate_url};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::redirect::Policy;
use std::error::Error as StdError;
use std::net::SocketAddr;

/// Redirect hops followed before giving up (reqwest's own default).
const MAX_REDIRECTS: usize = 10;

/// Longest error detail returned to callers.
const MAX_DETAIL_CHARS: usize = 300;

/// The target resolved only to addresses an egress gateway must not reach.
#[derive(Debug)]
pub struct BlockedTarget(String);

impl std::fmt::Display for BlockedTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "blocked target: {}", self.0)
    }
}

impl StdError for BlockedTarget {}

/// System resolver that drops loopback, private, link-local and other
/// non-public addresses (see [`is_blocked_ip`]).
pub struct PublicOnlyResolver;

impl Resolve for PublicOnlyResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let resolved: Vec<SocketAddr> =
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let public = public_only(resolved);
            if public.is_empty() {
                let err: Box<dyn StdError + Send + Sync> = Box::new(BlockedTarget(format!(
                    "{host} resolves only to non-public addresses"
                )));
                return Err(err);
            }
            let addrs: Addrs = Box::new(public.into_iter());
            Ok(addrs)
        })
    }
}

/// Keep only addresses an egress gateway may connect to. A host with both
/// public and private records keeps its public ones.
pub fn public_only(addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    addrs
        .into_iter()
        .filter(|addr| !is_blocked_ip(addr.ip()))
        .collect()
}

/// Follow redirects only to URLs that pass [`validate_url`].
pub fn redirect_policy() -> Policy {
    Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error(BlockedTarget(format!(
                "more than {MAX_REDIRECTS} redirects"
            )));
        }
        match validate_url(attempt.url().as_str()) {
            Ok(()) => attempt.follow(),
            Err(reason) => attempt.error(BlockedTarget(format!("redirect refused: {reason}"))),
        }
    })
}

/// `request failed [{kind}]: {detail}` for a failed upstream request, with
/// the URL removed and the detail bounded.
pub fn describe_fetch_error(err: reqwest::Error) -> String {
    let is_timeout = err.is_timeout();
    let is_connect = err.is_connect();
    let is_redirect = err.is_redirect();
    let err = err.without_url();

    let mut detail = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !detail.contains(&text) {
            detail.push_str(": ");
            detail.push_str(&text);
        }
        source = cause.source();
    }

    let kind = classify(&detail, is_timeout, is_connect, is_redirect);
    format!("request failed [{kind}]: {}", truncate(&detail))
}

/// Stable failure kind from the error chain. Callers branch on this, so the
/// strings are part of the API (see README "Errors").
fn classify(chain: &str, is_timeout: bool, is_connect: bool, is_redirect: bool) -> &'static str {
    let lower = chain.to_ascii_lowercase();
    if lower.contains("blocked target") {
        "blocked_target"
    } else if is_timeout {
        "timeout"
    } else if lower.contains("certificate") {
        "tls_certificate"
    } else if lower.contains("tls") || lower.contains("handshake") {
        "tls"
    } else if lower.contains("dns error") || lower.contains("failed to lookup address") {
        "dns"
    } else if is_redirect {
        "redirect"
    } else if is_connect {
        "connect"
    } else {
        "other"
    }
}

fn truncate(text: &str) -> String {
    match text.char_indices().nth(MAX_DETAIL_CHARS) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn addr(ip: IpAddr) -> SocketAddr {
        SocketAddr::new(ip, 443)
    }

    #[test]
    fn public_only_drops_private_and_keeps_public() {
        let kept = public_only(vec![
            addr(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))),
            addr(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))),
            addr(IpAddr::V6(Ipv6Addr::LOCALHOST)),
        ]);
        assert_eq!(
            kept,
            vec![addr(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)))]
        );
    }

    #[test]
    fn public_only_drops_mapped_loopback() {
        let mapped = Ipv4Addr::new(127, 0, 0, 1).to_ipv6_mapped();
        assert!(public_only(vec![addr(IpAddr::V6(mapped))]).is_empty());
    }

    #[tokio::test]
    async fn resolver_refuses_localhost() {
        let name: Name = "localhost".parse().expect("valid name");
        let err = match PublicOnlyResolver.resolve(name).await {
            Ok(_) => panic!("localhost must not resolve to a usable address"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("non-public"));
    }

    #[tokio::test]
    async fn redirect_to_metadata_address_is_refused() {
        use axum::{Router, http::header, response::IntoResponse, routing::get};

        let app = Router::new().route(
            "/",
            get(|| async {
                (
                    axum::http::StatusCode::FOUND,
                    [(header::LOCATION, "http://169.254.169.254/latest/meta-data/")],
                )
                    .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });

        let client = reqwest::Client::builder()
            .redirect(redirect_policy())
            .build()
            .expect("client");
        // The first hop is an IP literal, which skips the resolver; the guard
        // under test is the redirect policy.
        let err = client
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .expect_err("redirect must be refused");
        let message = describe_fetch_error(err);
        assert!(
            message.starts_with("request failed [blocked_target]:"),
            "{message}"
        );
        assert!(!message.contains("127.0.0.1:"), "URL leaked: {message}");
    }

    #[test]
    fn classify_maps_common_failures() {
        let cases = [
            (
                "blocked target: x resolves only to non-public addresses",
                false,
                true,
                false,
                "blocked_target",
            ),
            ("operation timed out", true, false, false, "timeout"),
            (
                "error sending request: client error (Connect): invalid peer certificate: Expired",
                false,
                true,
                false,
                "tls_certificate",
            ),
            (
                "error sending request: received fatal alert: HandshakeFailure",
                false,
                true,
                false,
                "tls",
            ),
            (
                "error sending request: dns error: failed to lookup address information",
                false,
                true,
                false,
                "dns",
            ),
            ("error following redirect", false, false, true, "redirect"),
            (
                "error sending request: tcp connect error: Connection refused",
                false,
                true,
                false,
                "connect",
            ),
            ("error decoding response body", false, false, false, "other"),
        ];
        for (chain, timeout, connect, redirect, expected) in cases {
            assert_eq!(
                classify(chain, timeout, connect, redirect),
                expected,
                "{chain}"
            );
        }
    }

    #[test]
    fn truncate_bounds_detail_on_char_boundary() {
        let long = "é".repeat(MAX_DETAIL_CHARS + 50);
        let cut = truncate(&long);
        assert_eq!(cut.chars().count(), MAX_DETAIL_CHARS + 1);
        assert!(cut.ends_with('…'));
    }
}
