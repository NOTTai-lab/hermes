//! Input validation for all incoming fetch requests.
//!
//! Two layers are enforced:
//! - **URL validation**: scheme allowlist + SSRF blocklist for private/reserved IP ranges.
//! - **Request validation**: upper bounds on `timeout_secs`, `crawl_delay_ms`, `ttl_secs`,
//!   and the number of forwarded headers, preventing resource exhaustion.

use crate::types::FetchRequest;
use std::net::IpAddr;

/// Validate a URL for use as a fetch target.
///
/// Rejects:
/// - Non-`http`/`https` schemes (prevents `file://`, `ftp://`, etc.).
/// - IP literals that map to loopback, private, link-local, CGNAT, or
///   documentation ranges (SSRF mitigation).
/// - The hostname `localhost` and common IPv6 loopback aliases.
///
/// Note: hostname-based SSRF (DNS rebinding) requires an outbound firewall or
/// runtime IP check at the OS level; this function handles the common cases.
pub fn validate_url(url_str: &str) -> Result<(), String> {
    let url = url::Url::parse(url_str).map_err(|e| format!("invalid URL: {e}"))?;

    match url.scheme() {
        "http" | "https" => {}
        scheme => return Err(format!("scheme '{scheme}' not allowed; use http or https")),
    }

    let host = match url.host_str() {
        Some(h) if !h.is_empty() => h,
        _ => return Err("URL has no host".to_string()),
    };

    if let Ok(ip) = host.parse::<IpAddr>()
        && is_blocked_ip(ip)
    {
        return Err(format!("target IP {ip} is not routable"));
    }

    let host_lower = host.to_lowercase();
    if matches!(
        host_lower.as_str(),
        "localhost" | "ip6-localhost" | "ip6-loopback"
    ) {
        return Err(format!("target host '{host}' is not allowed"));
    }

    Ok(())
}

/// Validate all tuneable fields of a [`FetchRequest`].
///
/// Enforces upper bounds to prevent request-level resource exhaustion:
///
/// | Field           | Maximum |
/// |-----------------|---------|
/// | `timeout_secs`  | 120 s   |
/// | `crawl_delay_ms`| 60 000 ms (1 min) |
/// | `ttl_secs`      | 86 400 s (24 h) |
/// | `headers` count | 50      |
pub fn validate_request(req: &FetchRequest) -> Result<(), String> {
    validate_url(&req.url)?;

    if req.timeout_secs == 0 || req.timeout_secs > 120 {
        return Err(format!(
            "timeout_secs must be 1–120, got {}",
            req.timeout_secs
        ));
    }

    if req.crawl_delay_ms > 60_000 {
        return Err(format!(
            "crawl_delay_ms must be ≤ 60 000, got {}",
            req.crawl_delay_ms
        ));
    }

    if req.ttl_secs > 86_400 {
        return Err(format!("ttl_secs must be ≤ 86 400, got {}", req.ttl_secs));
    }

    if req.headers.len() > 50 {
        return Err(format!(
            "too many request headers: max 50, got {}",
            req.headers.len()
        ));
    }

    Ok(())
}

/// Return `true` when `ip` belongs to a range that must not be reachable from
/// an egress gateway (loopback, private, link-local, cloud metadata, etc.).
///
/// Also used for resolved addresses by [`crate::guard::PublicOnlyResolver`].
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()           // 127.0.0.0/8
                || v4.is_private()     // 10/8, 172.16/12, 192.168/16
                || v4.is_link_local()  // 169.254/16
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()   // 224.0.0.0/4
                // "This network" (0.0.0.0/8)
                || o[0] == 0
                // CGNAT (100.64.0.0/10)
                || (o[0] == 100 && (o[1] & 0xC0) == 64)
                // Benchmarking (198.18.0.0/15)
                || (o[0] == 198 && (o[1] & 0xFE) == 18)
                // Reserved (240.0.0.0/4)
                || o[0] >= 240
        }
        IpAddr::V6(v6) => {
            // ::ffff:a.b.c.d carries an IPv4 address; judge that address.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_blocked_ip(IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Unique local fc00::/7
                || (v6.segments()[0] & 0xFE00) == 0xFC00
                // Link-local fe80::/10
                || (v6.segments()[0] & 0xFFC0) == 0xFE80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_public_https() {
        assert!(validate_url("https://example.com/page").is_ok());
    }

    #[test]
    fn rejects_loopback_ip() {
        assert!(validate_url("http://127.0.0.1/admin").is_err());
    }

    #[test]
    fn rejects_private_ip() {
        assert!(validate_url("http://10.0.0.1/").is_err());
        assert!(validate_url("http://192.168.1.1/").is_err());
    }

    #[test]
    fn rejects_metadata_ip() {
        assert!(validate_url("http://169.254.169.254/latest/meta-data/").is_err());
    }

    #[test]
    fn blocks_reserved_and_mapped_ranges() {
        for ip in [
            "0.1.2.3",
            "100.64.1.1",
            "198.18.0.1",
            "224.0.0.1",
            "240.0.0.1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "ff02::1",
            "fd00::1",
        ] {
            assert!(
                is_blocked_ip(ip.parse().expect("ip")),
                "{ip} must be blocked"
            );
        }
        for ip in [
            "93.184.216.34",
            "2606:2800:220:1:248:1893:25c8:1946",
            "::ffff:93.184.216.34",
        ] {
            assert!(
                !is_blocked_ip(ip.parse().expect("ip")),
                "{ip} must be allowed"
            );
        }
    }

    #[test]
    fn rejects_localhost_name() {
        assert!(validate_url("http://localhost/").is_err());
    }

    #[test]
    fn rejects_file_scheme() {
        assert!(validate_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn rejects_missing_host() {
        // "http://" has an empty authority; url crate sets host_str to Some("").
        assert!(validate_url("http://").is_err());
    }
}
