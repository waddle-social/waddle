//! SSRF-resilient avatar bytes fetcher for the OIDC bridge.
//!
//! Policy:
//!
//! - **Scheme:** `https://` only. `http`, `data:`, `file:` are rejected.
//! - **DNS pin:** the host is resolved once via the system resolver,
//!   every returned address is checked against the non-global block
//!   list, and reqwest is pinned to that address set via
//!   `resolve_to_addrs` so connect cannot race with a second
//!   resolution (DNS-rebinding TOCTOU defense).
//! - **Block list:** RFC 1918, loopback, link-local (incl.
//!   169.254/16), broadcast, documentation, unspecified, CGNAT
//!   (100.64/10), benchmark (198.18/15), `0.0.0.0/8`, reserved
//!   (240/4), and any IPv6 form of the above (incl. IPv4-mapped
//!   `::ffff:a.b.c.d` and IPv4-compatible `::a.b.c.d`), plus IPv6
//!   ULA, link-local, loopback, multicast, documentation
//!   (`2001:db8::/32`), and the `100::/64` discard prefix.
//! - **No auto-redirects:** 3xx responses are surfaced as
//!   [`FetchError::Http`]. A redirect target would otherwise need its
//!   own SSRF check on every hop, which can't be done in a sync
//!   `reqwest::redirect::Policy`. OIDC providers typically serve
//!   avatar URLs directly; if you see this in practice, file an
//!   issue.
//! - **Streaming size cap:** 100 KB hard cap, enforced chunk-by-chunk
//!   on the response body so an oversize/slowloris/lying-Content-Length
//!   server cannot OOM us.
//! - **MIME:** `image/png` only — XEP-0084 §3.1 restricts
//!   `urn:xmpp:avatar:data` to PNG. The header is checked, then the
//!   bytes are magic-byte sniffed against the PNG signature so a
//!   server lying in `Content-Type` can't smuggle non-PNG payloads
//!   downstream.
//! - **Timeouts:** 5s connect, 10s total.
//! - **Retries:** one retry on transient failures (5xx, connect
//!   timeout, network error). No backoff.
//!
//! All failure modes are typed via `FetchError` so callers can map
//! into the persisted `users.last_avatar_fetch_error` enum without
//! parsing log strings.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
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

/// XEP-0084 §3.1: `<data/>` is for `image/png` only.
const PNG_MIME: &str = "image/png";
/// PNG file signature (RFC 2083 §3.1).
const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

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
    #[error("response Content-Type {0:?} is not image/png")]
    MimeRejected(Option<String>),
    #[error("response body did not start with the PNG signature")]
    MagicByteMismatch,
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
            FetchError::MagicByteMismatch => "magic_byte_mismatch",
            FetchError::SizeExceeded(_) => "size_exceeded",
        }
    }
}

/// Knobs for the fetcher. Defaults match the production policy; tests
/// override the SSRF block to allow loopback when wiremock is the URL
/// host.
#[derive(Debug, Clone)]
pub struct FetchPolicy {
    pub max_bytes: usize,
    pub connect_timeout: Duration,
    pub total_timeout: Duration,
    /// When true, refuse RFC1918/loopback/link-local IPs. Tests serving
    /// avatars from wiremock on 127.0.0.1 set this to `false`.
    pub block_non_global_ips: bool,
    /// Production callers leave this `false` (HTTPS-only). Tests
    /// serving fixtures from wiremock (which speaks plain HTTP) flip
    /// it to `true` together with `block_non_global_ips=false`.
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
    let port = url.port_or_known_default().unwrap_or(443);

    // Resolve once, validate every returned address, and pin reqwest
    // to that exact set so the connect path cannot re-resolve (which
    // would open a TOCTOU/DNS-rebinding gap).
    let pinned_addrs: Vec<SocketAddr> = match host {
        Host::Ipv4(ip) => {
            if policy.block_non_global_ips && !is_global_ipv4(ip) {
                return Err(FetchError::SsrfBlocked(IpAddr::V4(ip)));
            }
            vec![SocketAddr::new(IpAddr::V4(ip), port)]
        }
        Host::Ipv6(ip) => {
            if policy.block_non_global_ips && !is_global_ipv6(ip) {
                return Err(FetchError::SsrfBlocked(IpAddr::V6(ip)));
            }
            vec![SocketAddr::new(IpAddr::V6(ip), port)]
        }
        Host::Domain(name) => {
            let resolved = resolve_host(name, port)
                .await
                .map_err(|e| FetchError::DnsResolution(e.to_string()))?;
            if resolved.is_empty() {
                return Err(FetchError::DnsResolution(format!(
                    "no addresses for {name}"
                )));
            }
            if policy.block_non_global_ips {
                for ip in &resolved {
                    let ok = match ip {
                        IpAddr::V4(v4) => is_global_ipv4(*v4),
                        IpAddr::V6(v6) => is_global_ipv6(*v6),
                    };
                    if !ok {
                        return Err(FetchError::SsrfBlocked(*ip));
                    }
                }
            }
            resolved
                .into_iter()
                .map(|ip| SocketAddr::new(ip, port))
                .collect()
        }
    };

    let mut builder = Client::builder()
        .connect_timeout(policy.connect_timeout)
        .timeout(policy.total_timeout)
        // No auto-redirects — a redirect target would need a fresh
        // SSRF check, which `redirect::Policy` (sync) cannot perform.
        .redirect(redirect::Policy::none())
        .https_only(!policy.allow_http_for_tests);
    if let Host::Domain(name) = host {
        builder = builder.resolve_to_addrs(name, &pinned_addrs);
    }
    let client = builder
        .build()
        .map_err(|e| FetchError::Network(e.to_string()))?;

    match try_fetch(&client, url, policy).await {
        Ok(ok) => Ok(ok),
        Err(error) => {
            let transient = matches!(error, FetchError::Network(_) | FetchError::Http(_))
                && error.kind() != "permanent_4xx";
            if !transient {
                return Err(error);
            }
            warn!(
                error = %error,
                url = %url,
                "transient avatar fetch failure; retrying once"
            );
            try_fetch(&client, url, policy).await
        }
    }
}

async fn try_fetch(
    client: &Client,
    url: &Url,
    policy: &FetchPolicy,
) -> Result<AvatarBytes, FetchError> {
    let mut response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        // 3xx surfaces as `Http(3xx)` because auto-redirects are off.
        return Err(FetchError::Http(status.as_u16()));
    }

    let header_mime = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or(v).trim().to_lowercase());
    if header_mime.as_deref() != Some(PNG_MIME) {
        return Err(FetchError::MimeRejected(header_mime));
    }

    if let Some(len) = response.content_length() {
        if (len as usize) > policy.max_bytes {
            return Err(FetchError::SizeExceeded(policy.max_bytes));
        }
    }

    // Stream chunk-by-chunk so a server that lies about Content-Length
    // (or doesn't set one) cannot make us buffer arbitrary data.
    let mut buf: Vec<u8> = Vec::with_capacity(policy.max_bytes.min(64 * 1024));
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?
    {
        if buf.len() + chunk.len() > policy.max_bytes {
            return Err(FetchError::SizeExceeded(policy.max_bytes));
        }
        buf.extend_from_slice(&chunk);
    }

    if buf.len() < PNG_MAGIC.len() || buf[..PNG_MAGIC.len()] != PNG_MAGIC {
        return Err(FetchError::MagicByteMismatch);
    }

    debug!(url = %url, bytes = buf.len(), mime = %PNG_MIME, "avatar fetched");
    Ok(AvatarBytes {
        bytes: buf,
        mime: PNG_MIME.to_string(),
    })
}

async fn resolve_host(host: &str, port: u16) -> std::io::Result<Vec<IpAddr>> {
    let addrs = tokio::net::lookup_host((host, port)).await?;
    Ok(addrs.map(|sa| sa.ip()).collect())
}

fn is_global_ipv4(ip: Ipv4Addr) -> bool {
    if ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
    {
        return false;
    }
    let octets = ip.octets();
    // 0.0.0.0/8 (RFC 1122 §3.2.1.3 — `0.0.0.0` is_unspecified, but
    // 0.0.0.1 etc. are not flagged by the std checks).
    if octets[0] == 0 {
        return false;
    }
    // CGNAT 100.64.0.0/10 (RFC 6598).
    if octets[0] == 100 && (octets[1] & 0xc0) == 0x40 {
        return false;
    }
    // Benchmarking 198.18.0.0/15 (RFC 2544).
    if octets[0] == 198 && (octets[1] & 0xfe) == 18 {
        return false;
    }
    // Reserved/future-use 240.0.0.0/4 (RFC 1112 §4).
    if octets[0] >= 240 {
        return false;
    }
    true
}

fn is_global_ipv6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        || ip.is_multicast()
    {
        return false;
    }
    // IPv4-mapped (`::ffff:a.b.c.d`) and IPv4-compatible (`::a.b.c.d`,
    // deprecated but still routable on some stacks). `to_ipv4`
    // matches both forms.
    if let Some(v4) = ip.to_ipv4() {
        return is_global_ipv4(v4);
    }
    let segs = ip.segments();
    // Site-local fec0::/10 (RFC 3879 — deprecated, but still routed
    // on some legacy stacks; defense in depth).
    if (segs[0] & 0xffc0) == 0xfec0 {
        return false;
    }
    // Documentation 2001:db8::/32 (RFC 3849).
    if segs[0] == 0x2001 && segs[1] == 0x0db8 {
        return false;
    }
    // Discard prefix 100::/64 (RFC 6666).
    if segs[0] == 0x0100 && segs[1] == 0 && segs[2] == 0 && segs[3] == 0 {
        return false;
    }
    true
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
    fn rejects_cgnat_v4() {
        let policy = FetchPolicy::default();
        let url: Url = "https://100.64.0.1/avatar.png".parse().unwrap();
        let result = futures::executor::block_on(fetch_avatar_bytes(&url, &policy));
        assert!(
            matches!(result, Err(FetchError::SsrfBlocked(_))),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_benchmark_v4() {
        let policy = FetchPolicy::default();
        let url: Url = "https://198.18.0.1/avatar.png".parse().unwrap();
        let result = futures::executor::block_on(fetch_avatar_bytes(&url, &policy));
        assert!(
            matches!(result, Err(FetchError::SsrfBlocked(_))),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_reserved_v4() {
        let policy = FetchPolicy::default();
        let url: Url = "https://240.0.0.1/avatar.png".parse().unwrap();
        let result = futures::executor::block_on(fetch_avatar_bytes(&url, &policy));
        assert!(
            matches!(result, Err(FetchError::SsrfBlocked(_))),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_zero_dot_zero_v4() {
        let policy = FetchPolicy::default();
        // 0.0.0.0 itself is_unspecified; 0.0.0.1 only matches with the
        // explicit 0.0.0.0/8 check.
        let url: Url = "https://0.0.0.1/avatar.png".parse().unwrap();
        let result = futures::executor::block_on(fetch_avatar_bytes(&url, &policy));
        assert!(
            matches!(result, Err(FetchError::SsrfBlocked(_))),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_ipv4_mapped_ipv6_loopback() {
        let policy = FetchPolicy::default();
        // `::ffff:127.0.0.1` — IPv4-mapped form of loopback. Without
        // the `to_ipv4` shortcut a dual-stack reqwest dials the
        // underlying IPv4 destination.
        let url: Url = "https://[::ffff:127.0.0.1]/avatar.png".parse().unwrap();
        let result = futures::executor::block_on(fetch_avatar_bytes(&url, &policy));
        assert!(
            matches!(result, Err(FetchError::SsrfBlocked(_))),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_ipv4_mapped_ipv6_metadata() {
        let policy = FetchPolicy::default();
        let url: Url = "https://[::ffff:169.254.169.254]/".parse().unwrap();
        let result = futures::executor::block_on(fetch_avatar_bytes(&url, &policy));
        assert!(
            matches!(result, Err(FetchError::SsrfBlocked(_))),
            "{result:?}"
        );
    }

    #[test]
    fn rejects_ipv6_loopback() {
        let policy = FetchPolicy::default();
        let url: Url = "https://[::1]/avatar.png".parse().unwrap();
        let result = futures::executor::block_on(fetch_avatar_bytes(&url, &policy));
        assert!(matches!(result, Err(FetchError::SsrfBlocked(_))));
    }

    #[test]
    fn rejects_ipv6_ula() {
        let policy = FetchPolicy::default();
        let url: Url = "https://[fc00::1]/avatar.png".parse().unwrap();
        let result = futures::executor::block_on(fetch_avatar_bytes(&url, &policy));
        assert!(matches!(result, Err(FetchError::SsrfBlocked(_))));
    }

    #[test]
    fn rejects_ipv6_site_local() {
        let policy = FetchPolicy::default();
        // fec0::/10 — RFC 3879 deprecated site-local, still routed on
        // some legacy IPv6 stacks. None of the std `is_*` predicates
        // catch it.
        let url: Url = "https://[fec0::1]/avatar.png".parse().unwrap();
        let result = futures::executor::block_on(fetch_avatar_bytes(&url, &policy));
        assert!(
            matches!(result, Err(FetchError::SsrfBlocked(_))),
            "{result:?}"
        );
    }

    #[test]
    fn fetch_error_kind_classifies_correctly() {
        assert_eq!(FetchError::Http(404).kind(), "permanent_4xx");
        assert_eq!(FetchError::Http(503).kind(), "transient_5xx");
        assert_eq!(FetchError::SizeExceeded(100).kind(), "size_exceeded");
        assert_eq!(FetchError::MagicByteMismatch.kind(), "magic_byte_mismatch");
        assert_eq!(
            FetchError::MimeRejected(Some("application/pdf".into())).kind(),
            "mime_rejected"
        );
    }
}
