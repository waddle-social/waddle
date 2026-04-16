//! HTTP client with custom DNS-resolver SSRF filter.
//!
//! Wraps `reqwest::Client` with a [`SafeResolver`] that asks
//! `hickory-resolver` for A/AAAA records and drops any result pointing at
//! a disallowed IP per [`crate::ssrf::is_disallowed_ip`]. Combined with
//! literal-IP checks on URL host parts at call time, this prevents the
//! classic "public DNS name with a private A record" SSRF bypass.
//!
//! Other guardrails enforced by [`fetch_preview`]:
//! - `http`/`https` schemes only.
//! - 5-second default timeout.
//! - 512 KiB default body cap (mid-stream abort).
//! - `Content-Type` allowlist: `text/html`, `application/xhtml+xml`, `image/*`.
//! - Max 3 redirects, each hop re-validated through the same URL filter.
//! - No credentials, no cookies, no auth headers.
//! - `User-Agent` set to identify the Waddle crawler so site admins have
//!   a contact surface.

use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use hickory_resolver::{
    config::{ResolverConfig, ResolverOpts},
    name_server::TokioConnectionProvider,
    Resolver, TokioResolver,
};
use reqwest::{
    dns::{Addrs, Name, Resolve, Resolving},
    redirect::Policy,
    Client,
};
use thiserror::Error;
use tracing::debug;
use url::Url;

use crate::html::parse_html;
use crate::ssrf::is_disallowed_ip;
use crate::{LinkPreview, LinkPreviewImage};

/// Outcome of a successful fetch.
#[derive(Debug)]
pub enum FetchOutcome {
    /// HTML parsed into a preview.
    Html(LinkPreview),
    /// Direct-image URL — preview carries only the image reference.
    Image(LinkPreview),
}

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("url is not http/https")]
    BadScheme,
    #[error("url host resolved to a disallowed address")]
    PrivateHost,
    #[error("url host parse failed")]
    BadUrl,
    #[error("timed out")]
    Timeout,
    #[error("upstream responded {0}")]
    BadStatus(u16),
    #[error("content-type not allowed: {0}")]
    BadContentType(String),
    #[error("body exceeded {0} bytes")]
    TooLarge(usize),
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("transport error: {0}")]
    Transport(String),
}

/// Runtime knobs for the HTTP client.
#[derive(Debug, Clone)]
pub struct FetchConfig {
    pub user_agent: String,
    pub timeout: Duration,
    pub max_bytes: usize,
    pub max_redirects: usize,
    /// Test-only escape hatch: when `true`, both the literal-IP guard
    /// and the DNS-resolver filter allow private/loopback addresses.
    /// Production must leave this `false`.
    pub allow_private_addresses: bool,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            user_agent: "Waddle-LinkPreview/0.1 (+https://waddle.chat)".to_owned(),
            timeout: Duration::from_secs(5),
            max_bytes: 512 * 1024,
            max_redirects: 3,
            allow_private_addresses: false,
        }
    }
}

/// Build a reqwest client backed by a private-IP-filtering DNS resolver.
pub fn build_client(config: &FetchConfig) -> Result<Client, FetchError> {
    let resolver = Arc::new(SafeResolver::production()?);
    build_client_with_resolver(config, resolver)
}

/// Test hook: build a client that allows every resolved IP through the
/// filter. Used by wiremock-backed integration tests where the upstream
/// is necessarily on loopback.
pub fn build_client_allow_private(config: &FetchConfig) -> Result<Client, FetchError> {
    let resolver = Arc::new(SafeResolver::allow_all()?);
    build_client_with_resolver(config, resolver)
}

fn build_client_with_resolver(
    config: &FetchConfig,
    resolver: Arc<SafeResolver>,
) -> Result<Client, FetchError> {
    Client::builder()
        .user_agent(config.user_agent.clone())
        .timeout(config.timeout)
        .redirect(Policy::none())
        .dns_resolver(resolver)
        .build()
        .map_err(|e: reqwest::Error| FetchError::Transport(e.to_string()))
}

/// Fetch a preview for `target`, following up to `config.max_redirects`
/// redirects and re-validating every hop.
pub async fn fetch_preview(
    client: &Client,
    target: Url,
    config: &FetchConfig,
) -> Result<FetchOutcome, FetchError> {
    validate_target_url(&target, config.allow_private_addresses)?;
    let mut current = target;
    let mut redirects_left = config.max_redirects;

    loop {
        let resp = match client.get(current.clone()).send().await {
            Ok(r) => r,
            Err(e) => {
                if e.is_timeout() {
                    return Err(FetchError::Timeout);
                }
                return Err(FetchError::Transport(e.to_string()));
            }
        };

        let status = resp.status().as_u16();
        if (300..400).contains(&status) && status != 304 {
            let Some(location) = resp.headers().get(reqwest::header::LOCATION) else {
                return Err(FetchError::BadStatus(status));
            };
            let location_str = location
                .to_str()
                .map_err(|_| FetchError::Transport("invalid location header".to_owned()))?;
            let next = current
                .join(location_str)
                .map_err(|_| FetchError::BadUrl)?;
            if redirects_left == 0 {
                return Err(FetchError::TooManyRedirects);
            }
            redirects_left -= 1;
            validate_target_url(&next, config.allow_private_addresses)?;
            current = next;
            continue;
        }

        if !(200..300).contains(&status) {
            return Err(FetchError::BadStatus(status));
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(normalize_content_type)
            .unwrap_or_default();

        match classify_content_type(&content_type) {
            ContentKind::Image => {
                return Ok(FetchOutcome::Image(image_only_preview(&current)));
            }
            ContentKind::Html => {
                // fall through
            }
            ContentKind::Rejected => {
                return Err(FetchError::BadContentType(content_type));
            }
        }

        if let Some(len) = resp.content_length() {
            if (len as usize) > config.max_bytes {
                return Err(FetchError::TooLarge(config.max_bytes));
            }
        }

        let body = read_capped_body(resp, config.max_bytes).await?;
        let html = String::from_utf8_lossy(&body);
        let preview = parse_html(&html, &current);
        return Ok(FetchOutcome::Html(preview));
    }
}

fn image_only_preview(url: &Url) -> LinkPreview {
    LinkPreview {
        url: url.to_string(),
        canonical_url: Some(url.to_string()),
        title: None,
        description: None,
        site_name: url
            .host_str()
            .map(|h| h.strip_prefix("www.").unwrap_or(h).to_owned()),
        type_: Some("image".to_owned()),
        image: Some(LinkPreviewImage {
            src: url.to_string(),
            width: None,
            height: None,
        }),
    }
}

/// Validate a URL before any network egress: http/https + literal-IP
/// host check. Public hostnames pass here; the DNS resolver does the
/// second-stage filter.
fn validate_target_url(url: &Url, allow_private: bool) -> Result<(), FetchError> {
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err(FetchError::BadScheme),
    }
    let Some(host) = url.host() else {
        return Err(FetchError::BadUrl);
    };
    if allow_private {
        return Ok(());
    }
    match host {
        url::Host::Ipv4(ip) => {
            if is_disallowed_ip(ip.into()) {
                return Err(FetchError::PrivateHost);
            }
        }
        url::Host::Ipv6(ip) => {
            if is_disallowed_ip(ip.into()) {
                return Err(FetchError::PrivateHost);
            }
        }
        url::Host::Domain(_) => {}
    }
    Ok(())
}

/// Content type bucket.
#[derive(Debug, PartialEq, Eq)]
pub enum ContentKind {
    Html,
    Image,
    Rejected,
}

pub fn classify_content_type(normalized: &str) -> ContentKind {
    match normalized {
        "text/html" | "application/xhtml+xml" => ContentKind::Html,
        other if other.starts_with("image/") => ContentKind::Image,
        _ => ContentKind::Rejected,
    }
}

/// Strip parameters and lowercase a Content-Type value.
pub fn normalize_content_type(raw: &str) -> String {
    let semi = raw.find(';').unwrap_or(raw.len());
    raw[..semi].trim().to_ascii_lowercase()
}

async fn read_capped_body(
    resp: reqwest::Response,
    max_bytes: usize,
) -> Result<Bytes, FetchError> {
    let mut total = 0usize;
    let mut buf = BytesMut::with_capacity(max_bytes.min(64 * 1024));
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e: reqwest::Error| FetchError::Transport(e.to_string()))?;
        total += chunk.len();
        if total > max_bytes {
            return Err(FetchError::TooLarge(max_bytes));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf.freeze())
}

/// A `reqwest::dns::Resolve` implementation that filters A/AAAA results
/// through [`is_disallowed_ip`] before returning them to the connector.
pub struct SafeResolver {
    inner: TokioResolver,
    allow_private: bool,
}

impl SafeResolver {
    pub fn production() -> Result<Self, FetchError> {
        Self::build(false)
    }

    fn allow_all() -> Result<Self, FetchError> {
        Self::build(true)
    }

    fn build(allow_private: bool) -> Result<Self, FetchError> {
        let inner = Resolver::builder_with_config(
            ResolverConfig::cloudflare(),
            TokioConnectionProvider::default(),
        )
        .with_options(ResolverOpts::default())
        .build();
        Ok(Self {
            inner,
            allow_private,
        })
    }
}

impl Resolve for SafeResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = self.inner.clone();
        let allow_private = self.allow_private;
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let lookup = match resolver.lookup_ip(host.as_str()).await {
                Ok(l) => l,
                Err(e) => return Err(Box::new(e) as _),
            };
            let addrs = filter_addresses(lookup.into_iter(), allow_private);
            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::other(
                    "all resolved addresses disallowed",
                )) as _);
            }
            let iter: Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

/// Drop disallowed IPs from a list of resolved addresses, wrapping each
/// in a `SocketAddr` with port 0 (reqwest overrides the port from the URL).
pub fn filter_addresses(
    ips: impl Iterator<Item = IpAddr>,
    allow_private: bool,
) -> Vec<SocketAddr> {
    ips.filter(|ip| allow_private || !is_disallowed_ip(*ip))
        .map(|ip| SocketAddr::new(ip, 0))
        .collect()
}

// `Name::from_str` is used by consumers that build their own Name objects.
// Re-exported to ensure the crate's public surface doesn't depend on
// reqwest internals leaking in tests.
#[doc(hidden)]
pub fn parse_name(host: &str) -> Option<Name> {
    Name::from_str(host).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_html() {
        assert_eq!(classify_content_type("text/html"), ContentKind::Html);
        assert_eq!(classify_content_type("application/xhtml+xml"), ContentKind::Html);
    }

    #[test]
    fn classify_image() {
        assert_eq!(classify_content_type("image/png"), ContentKind::Image);
        assert_eq!(classify_content_type("image/jpeg"), ContentKind::Image);
        assert_eq!(classify_content_type("image/webp"), ContentKind::Image);
    }

    #[test]
    fn classify_rejected() {
        assert_eq!(classify_content_type("application/json"), ContentKind::Rejected);
        assert_eq!(classify_content_type("application/zip"), ContentKind::Rejected);
        assert_eq!(classify_content_type(""), ContentKind::Rejected);
        assert_eq!(classify_content_type("text/plain"), ContentKind::Rejected);
    }

    #[test]
    fn normalize_strips_charset_and_lowercases() {
        assert_eq!(normalize_content_type("text/html; charset=utf-8"), "text/html");
        assert_eq!(normalize_content_type("TEXT/HTML"), "text/html");
        assert_eq!(normalize_content_type("  image/PNG ; x=1"), "image/png");
    }

    #[test]
    fn validate_accepts_public_http_and_https() {
        assert!(validate_target_url(&Url::parse("http://example.com/").unwrap(), false).is_ok());
        assert!(validate_target_url(&Url::parse("https://example.com/").unwrap(), false).is_ok());
        assert!(validate_target_url(&Url::parse("https://8.8.8.8/").unwrap(), false).is_ok());
    }

    #[test]
    fn validate_rejects_non_http_schemes() {
        assert!(matches!(
            validate_target_url(&Url::parse("file:///etc/passwd").unwrap(), false),
            Err(FetchError::BadScheme)
        ));
        assert!(matches!(
            validate_target_url(&Url::parse("ftp://x.example/").unwrap(), false),
            Err(FetchError::BadScheme)
        ));
    }

    #[test]
    fn validate_rejects_literal_private_ip() {
        assert!(matches!(
            validate_target_url(&Url::parse("http://192.168.1.1/").unwrap(), false),
            Err(FetchError::PrivateHost)
        ));
        assert!(matches!(
            validate_target_url(&Url::parse("http://127.0.0.1/").unwrap(), false),
            Err(FetchError::PrivateHost)
        ));
        assert!(matches!(
            validate_target_url(&Url::parse("http://169.254.169.254/").unwrap(), false),
            Err(FetchError::PrivateHost)
        ));
    }

    #[test]
    fn validate_rejects_literal_private_ipv6() {
        assert!(matches!(
            validate_target_url(&Url::parse("http://[::1]/").unwrap(), false),
            Err(FetchError::PrivateHost)
        ));
        assert!(matches!(
            validate_target_url(&Url::parse("http://[fe80::1]/").unwrap(), false),
            Err(FetchError::PrivateHost)
        ));
    }

    #[test]
    fn filter_drops_private_v4() {
        let ips = ["10.0.0.1", "8.8.8.8", "127.0.0.1", "1.1.1.1"]
            .into_iter()
            .map(|s| s.parse().unwrap());
        let out = filter_addresses(ips, false);
        let hosts: Vec<IpAddr> = out.iter().map(|s| s.ip()).collect();
        assert!(hosts.contains(&"8.8.8.8".parse::<IpAddr>().unwrap()));
        assert!(hosts.contains(&"1.1.1.1".parse::<IpAddr>().unwrap()));
        assert!(!hosts.contains(&"10.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(!hosts.contains(&"127.0.0.1".parse::<IpAddr>().unwrap()));
    }

    #[test]
    fn filter_allow_private_keeps_everything() {
        let ips = ["10.0.0.1", "127.0.0.1"]
            .into_iter()
            .map(|s| s.parse().unwrap());
        let out = filter_addresses(ips, true);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn filter_port_is_zero_for_reqwest_to_override() {
        let ips = ["8.8.8.8"].into_iter().map(|s| s.parse().unwrap());
        let out = filter_addresses(ips, false);
        assert_eq!(out[0].port(), 0);
    }

    #[test]
    fn image_only_preview_carries_only_image_fields() {
        let url = Url::parse("https://cdn.example.com/photo.png").unwrap();
        let p = image_only_preview(&url);
        assert_eq!(p.type_.as_deref(), Some("image"));
        assert_eq!(p.image.as_ref().unwrap().src, "https://cdn.example.com/photo.png");
        assert_eq!(p.site_name.as_deref(), Some("cdn.example.com"));
        assert!(p.title.is_none());
    }
}

// Silence dead-code warning for test-only helper when compiled without tests.
#[doc(hidden)]
pub fn _touch() {
    debug!("link-preview fetch helpers compiled");
}
