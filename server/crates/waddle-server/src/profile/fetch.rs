//! SSRF-resilient avatar bytes fetcher for the OIDC bridge.
//!
//! Policy (RFC 363 PR 3):
//!
//! - **Scheme:** `https://` only. `http`, `data:`, `file:` are rejected.
//! - **DNS rebind / SSRF:** after DNS resolution, refuse RFC 1918,
//!   loopback, link-local (incl. 169.254/16), and non-global IPv6.
//! - **Size cap:** 100 KB hard cap on the raw bytes (transitional —
//!   lifted to 1 MB by issue #437 binary-payload object storage).
//! - **MIME allowlist:** `image/png`, `image/jpeg`, `image/gif`,
//!   `image/webp`. Anything else is `MimeRejected`.
//! - **Timeouts:** 5s connect, 10s total.
//! - **Retries:** one retry on transient failures (5xx, connect
//!   timeout, network error). No backoff.
//!
//! All failure modes are typed via `FetchError` so callers can map
//! into the persisted `users.last_avatar_fetch_error` enum without
//! parsing log strings.

use std::net::IpAddr;
use std::time::Duration;

use reqwest::header::CONTENT_TYPE;
use reqwest::redirect;
use reqwest::Client;
use thiserror::Error;
use tracing::{debug, warn};
use url::{Host, Url};

const MAX_BYTES: usize = 100 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);

/// MIME types accepted for `urn:xmpp:avatar:data` payloads.
const ALLOWED_MIMES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Successful avatar fetch result.
#[derive(Debug, Clone)]
pub struct AvatarBytes {
    pub bytes: Vec<u8>,
    pub mime: String,
}

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("URL scheme must be https; got {0}")]
    InvalidScheme(String),
    #[error("URL has no host")]
    MissingHost,
    #[error("DNS resolution failed: {0}")]
    DnsResolution(String),
    #[error("SSRF: target IP {0} is not a global address")]
    SsrfBlocked(IpAddr),
    #[error("transport error: {0}")]
    Network(String),
    #[error("HTTP {0}")]
    Http(u16),
    #[error("response Content-Type {0:?} is not in the allowlist")]
    MimeRejected(Option<String>),
    #[error("response exceeds {0}-byte cap")]
    SizeExceeded(usize),
}

impl FetchError {
    /// Classification used to populate `users.last_avatar_fetch_error`.
    pub fn kind(&self) -> &'static str {
        match self {
            FetchError::InvalidScheme(_) => "invalid_scheme",
            FetchError::MissingHost => "missing_host",
            FetchError::DnsResolution(_) => "dns",
            FetchError::SsrfBlocked(_) => "ssrf_blocked",
            FetchError::Network(_) => "network",
            FetchError::Http(code) if *code >= 500 => "transient_5xx",
            FetchError::Http(_) => "permanent_4xx",
            FetchError::MimeRejected(_) => "mime_rejected",
            FetchError::SizeExceeded(_) => "size_exceeded",
        }
    }
}

/// Knobs for the fetcher. Defaults match the RFC; tests override the
/// SSRF block to allow loopback when wiremock is the URL host.
#[derive(Debug, Clone)]
pub struct FetchPolicy {
    pub max_bytes: usize,
    pub connect_timeout: Duration,
    pub total_timeout: Duration,
    /// When true, refuse RFC1918/loopback/link-local IPs. Tests serving
    /// avatars from wiremock on 127.0.0.1 set this to `false`.
    pub block_non_global_ips: bool,
    /// Production callers leave this `false` (HTTPS-only per RFC 363).
    /// Tests serving fixtures from wiremock (which speaks plain HTTP)
    /// flip it to `true` together with `block_non_global_ips=false`.
    pub allow_http_for_tests: bool,
}

impl Default for FetchPolicy {
    fn default() -> Self {
        Self {
            max_bytes: MAX_BYTES,
            connect_timeout: CONNECT_TIMEOUT,
            total_timeout: TOTAL_TIMEOUT,
            block_non_global_ips: true,
            allow_http_for_tests: false,
        }
    }
}

/// Fetch the bytes at `url` per the policy.
pub async fn fetch_avatar_bytes(
    url: &Url,
    policy: &FetchPolicy,
) -> Result<AvatarBytes, FetchError> {
    let scheme_ok =
        url.scheme() == "https" || (policy.allow_http_for_tests && url.scheme() == "http");
    if !scheme_ok {
        return Err(FetchError::InvalidScheme(url.scheme().to_string()));
    }
    let host = url.host().ok_or(FetchError::MissingHost)?;

    if policy.block_non_global_ips {
        match host {
            Host::Ipv4(ip) => {
                if !is_global_ipv4(ip) {
                    return Err(FetchError::SsrfBlocked(IpAddr::V4(ip)));
                }
            }
            Host::Ipv6(ip) => {
                if !is_global_ipv6(ip) {
                    return Err(FetchError::SsrfBlocked(IpAddr::V6(ip)));
                }
            }
            Host::Domain(name) => {
                let resolved = resolve_host(name, url.port_or_known_default().unwrap_or(443))
                    .await
                    .map_err(|e| FetchError::DnsResolution(e.to_string()))?;
                for ip in &resolved {
                    let ok = match ip {
                        IpAddr::V4(v4) => is_global_ipv4(*v4),
                        IpAddr::V6(v6) => is_global_ipv6(*v6),
                    };
                    if !ok {
                        return Err(FetchError::SsrfBlocked(*ip));
                    }
                }
                if resolved.is_empty() {
                    return Err(FetchError::DnsResolution(format!(
                        "no addresses for {name}"
                    )));
                }
            }
        }
    }

    let client = Client::builder()
        .connect_timeout(policy.connect_timeout)
        .timeout(policy.total_timeout)
        .redirect(redirect::Policy::limited(3))
        .https_only(!policy.allow_http_for_tests)
        .build()
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let mut last_error: Option<FetchError> = None;
    for attempt in 0..2u8 {
        match try_fetch(&client, url, policy).await {
            Ok(ok) => return Ok(ok),
            Err(error) => {
                let transient = matches!(error, FetchError::Network(_) | FetchError::Http(_))
                    && error.kind() != "permanent_4xx";
                if !transient || attempt == 1 {
                    return Err(error);
                }
                warn!(
                    error = %error,
                    attempt,
                    url = %url,
                    "transient avatar fetch failure; retrying once"
                );
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| FetchError::Network("retry exhausted".into())))
}

async fn try_fetch(
    client: &Client,
    url: &Url,
    policy: &FetchPolicy,
) -> Result<AvatarBytes, FetchError> {
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Http(status.as_u16()));
    }

    let mime = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or(v).trim().to_lowercase());
    match mime.as_deref() {
        Some(m) if ALLOWED_MIMES.contains(&m) => {}
        other => return Err(FetchError::MimeRejected(other.map(str::to_string))),
    }

    if let Some(len) = response.content_length() {
        if (len as usize) > policy.max_bytes {
            return Err(FetchError::SizeExceeded(policy.max_bytes));
        }
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    if bytes.len() > policy.max_bytes {
        return Err(FetchError::SizeExceeded(policy.max_bytes));
    }

    let mime = mime.unwrap_or_default();
    debug!(url = %url, bytes = bytes.len(), %mime, "avatar fetched");
    Ok(AvatarBytes {
        bytes: bytes.to_vec(),
        mime,
    })
}

async fn resolve_host(host: &str, port: u16) -> std::io::Result<Vec<IpAddr>> {
    let addrs = tokio::net::lookup_host((host, port)).await?;
    Ok(addrs.map(|sa| sa.ip()).collect())
}

fn is_global_ipv4(ip: std::net::Ipv4Addr) -> bool {
    !ip.is_loopback()
        && !ip.is_private()
        && !ip.is_link_local()
        && !ip.is_broadcast()
        && !ip.is_documentation()
        && !ip.is_unspecified()
        && !is_ipv4_aws_metadata(ip)
}

fn is_ipv4_aws_metadata(ip: std::net::Ipv4Addr) -> bool {
    ip.octets()[0] == 169 && ip.octets()[1] == 254
}

fn is_global_ipv6(ip: std::net::Ipv6Addr) -> bool {
    !ip.is_loopback()
        && !ip.is_unspecified()
        && !ip.is_unique_local()
        && !ip.is_unicast_link_local()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_https_scheme() {
        let policy = FetchPolicy::default();
        let url: Url = "http://example.com/avatar.png".parse().unwrap();
        let result = futures::executor::block_on(fetch_avatar_bytes(&url, &policy));
        assert!(matches!(result, Err(FetchError::InvalidScheme(_))));
    }

    #[test]
    fn rejects_loopback_v4() {
        let policy = FetchPolicy::default();
        let url: Url = "https://127.0.0.1/avatar.png".parse().unwrap();
        let result = futures::executor::block_on(fetch_avatar_bytes(&url, &policy));
        assert!(
            matches!(result, Err(FetchError::SsrfBlocked(_))),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_rfc1918_v4() {
        let policy = FetchPolicy::default();
        let url: Url = "https://10.0.0.1/avatar.png".parse().unwrap();
        let result = futures::executor::block_on(fetch_avatar_bytes(&url, &policy));
        assert!(matches!(result, Err(FetchError::SsrfBlocked(_))));
    }

    #[test]
    fn rejects_link_local_169_254() {
        let policy = FetchPolicy::default();
        let url: Url = "https://169.254.169.254/latest/meta-data/".parse().unwrap();
        let result = futures::executor::block_on(fetch_avatar_bytes(&url, &policy));
        assert!(matches!(result, Err(FetchError::SsrfBlocked(_))));
    }

    #[test]
    fn fetch_error_kind_classifies_correctly() {
        assert_eq!(FetchError::Http(404).kind(), "permanent_4xx");
        assert_eq!(FetchError::Http(503).kind(), "transient_5xx");
        assert_eq!(FetchError::SizeExceeded(100).kind(), "size_exceeded");
        assert_eq!(
            FetchError::MimeRejected(Some("application/pdf".into())).kind(),
            "mime_rejected"
        );
    }
}
