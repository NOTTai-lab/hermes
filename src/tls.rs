//! Facts about the certificate the upstream server presented.
//!
//! Callers that watch a site's certificate (expiry, issuer changes) cannot
//! open TLS connections themselves: all outbound traffic goes through Hermes.
//! So Hermes reports what it saw on the connection that served the response.
//! Only the leaf certificate is read, and only fields a caller can act on.

use crate::types::TlsCertificate;
use chrono::DateTime;
use sha2::{Digest, Sha256};
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::{FromDer, X509Certificate};

/// SAN entries returned at most; a CDN certificate can list hundreds.
const MAX_DNS_NAMES: usize = 100;

/// Longest single name or issuer string returned.
const MAX_NAME_CHARS: usize = 253;

/// Parse a DER leaf certificate. `None` when it cannot be parsed; a fetch
/// never fails because of this.
pub fn parse_peer_certificate(der: &[u8]) -> Option<TlsCertificate> {
    let (_, cert) = X509Certificate::from_der(der).ok()?;
    let validity = cert.validity();
    let not_before = DateTime::from_timestamp(validity.not_before.timestamp(), 0)?;
    let not_after = DateTime::from_timestamp(validity.not_after.timestamp(), 0)?;

    let issuer = cert
        .issuer()
        .iter_organization()
        .chain(cert.issuer().iter_common_name())
        .find_map(|attr| attr.as_str().ok())
        .map(bounded);
    let subject = cert
        .subject()
        .iter_common_name()
        .find_map(|attr| attr.as_str().ok())
        .map(bounded);

    let dns_names = match cert.subject_alternative_name() {
        Ok(Some(san)) => san
            .value
            .general_names
            .iter()
            .filter_map(|name| match name {
                GeneralName::DNSName(dns) => Some(bounded(dns)),
                _ => None,
            })
            .take(MAX_DNS_NAMES)
            .collect(),
        _ => Vec::new(),
    };

    Some(TlsCertificate {
        not_before: not_before.to_rfc3339(),
        not_after: not_after.to_rfc3339(),
        issuer,
        subject,
        dns_names,
        sha256: hex::encode(Sha256::digest(der)),
    })
}

fn bounded(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_NAME_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Self-signed test certificate: CN=example.test, O=Hermes Test CA,
    /// SAN example.test + www.example.test, valid 30 days from 2026-11-01.
    const CERT_DER: &[u8] = include_bytes!("../tests/fixtures/example-test-cert.der");

    #[test]
    fn reads_validity_issuer_and_names() {
        let cert = parse_peer_certificate(CERT_DER).expect("fixture parses");
        assert_eq!(cert.issuer.as_deref(), Some("Hermes Test CA"));
        assert_eq!(cert.subject.as_deref(), Some("example.test"));
        assert_eq!(cert.dns_names, vec!["example.test", "www.example.test"]);
        assert!(cert.not_before < cert.not_after);
        assert_eq!(cert.sha256.len(), 64);
    }

    #[test]
    fn garbage_is_none() {
        assert!(parse_peer_certificate(b"not a certificate").is_none());
        assert!(parse_peer_certificate(&[]).is_none());
    }
}
