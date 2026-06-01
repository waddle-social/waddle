use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, LOCATION};
use reqwest::{redirect, Client, StatusCode};
use url::{Host, Url};

const DEFAULT_MAX_BYTES: usize = 256 * 1024;
const DEFAULT_MAX_REDIRECTS: usize = 3;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedLinkMetadata {
    pub original_url: Url,
    pub normalized_url: Url,
    pub title: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct LinkPreviewResolverPolicy {
    pub max_bytes: usize,
    pub max_redirects: usize,
    pub timeout: Duration,
    pub allow_http_loopback_for_tests: bool,
}

impl Default for LinkPreviewResolverPolicy {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            timeout: DEFAULT_TIMEOUT,
            allow_http_loopback_for_tests: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LinkPreviewResolverStatus {
    Ready,
    Blocked,
    Failed,
    Unsupported,
}

impl LinkPreviewResolverStatus {
    pub(super) fn as_lookup_status(self) -> &'static str {
        match self {
            LinkPreviewResolverStatus::Ready => "ready",
            LinkPreviewResolverStatus::Blocked => "blocked",
            LinkPreviewResolverStatus::Failed => "failed",
            LinkPreviewResolverStatus::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum LinkPreviewResolverOutcome {
    Ready(Box<ResolvedLinkMetadata>),
    Blocked,
    Failed,
    Unsupported,
}

impl LinkPreviewResolverOutcome {
    pub(super) fn status(&self) -> LinkPreviewResolverStatus {
        match self {
            LinkPreviewResolverOutcome::Ready(_) => LinkPreviewResolverStatus::Ready,
            LinkPreviewResolverOutcome::Blocked => LinkPreviewResolverStatus::Blocked,
            LinkPreviewResolverOutcome::Failed => LinkPreviewResolverStatus::Failed,
            LinkPreviewResolverOutcome::Unsupported => LinkPreviewResolverStatus::Unsupported,
        }
    }
}

pub(super) async fn resolve_link_preview(
    url: &Url,
    policy: &LinkPreviewResolverPolicy,
) -> LinkPreviewResolverOutcome {
    let mut current = url.clone();
    for redirect_count in 0..=policy.max_redirects {
        let fetch = match fetch_html_once(&current, policy).await {
            Ok(fetch) => fetch,
            Err(status) => return status.into(),
        };
        match fetch {
            FetchOnceResult::Html { final_url, html } => {
                return extract_metadata_from_html(url, &html)
                    .map(|mut metadata| {
                        metadata.normalized_url = final_url;
                        LinkPreviewResolverOutcome::Ready(Box::new(metadata))
                    })
                    .unwrap_or(LinkPreviewResolverOutcome::Unsupported);
            }
            FetchOnceResult::Redirect(next) => {
                if redirect_count == policy.max_redirects {
                    return LinkPreviewResolverOutcome::Failed;
                }
                current = next;
            }
        }
    }
    LinkPreviewResolverOutcome::Failed
}

impl From<LinkPreviewResolverStatus> for LinkPreviewResolverOutcome {
    fn from(status: LinkPreviewResolverStatus) -> Self {
        match status {
            LinkPreviewResolverStatus::Ready => LinkPreviewResolverOutcome::Failed,
            LinkPreviewResolverStatus::Blocked => LinkPreviewResolverOutcome::Blocked,
            LinkPreviewResolverStatus::Failed => LinkPreviewResolverOutcome::Failed,
            LinkPreviewResolverStatus::Unsupported => LinkPreviewResolverOutcome::Unsupported,
        }
    }
}

enum FetchOnceResult {
    Html { final_url: Url, html: String },
    Redirect(Url),
}

async fn fetch_html_once(
    url: &Url,
    policy: &LinkPreviewResolverPolicy,
) -> Result<FetchOnceResult, LinkPreviewResolverStatus> {
    let target = prepare_target(url, policy).await?;
    let mut builder = Client::builder()
        .timeout(policy.timeout)
        .connect_timeout(policy.timeout)
        .redirect(redirect::Policy::none())
        .https_only(!policy.allow_http_loopback_for_tests);
    if let Host::Domain(host) = target.host {
        builder = builder.resolve_to_addrs(host, &target.addrs);
    }
    let client = builder
        .build()
        .map_err(|_| LinkPreviewResolverStatus::Failed)?;
    let mut response = client
        .get(url.clone())
        .send()
        .await
        .map_err(|_| LinkPreviewResolverStatus::Failed)?;

    if response.status().is_redirection() {
        let Some(location) = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
        else {
            return Err(LinkPreviewResolverStatus::Failed);
        };
        let next = url
            .join(location)
            .map_err(|_| LinkPreviewResolverStatus::Failed)?;
        let status = classify_url_with_policy(&next, policy);
        if status != LinkPreviewResolverStatus::Ready {
            return Err(status);
        }
        return Ok(FetchOnceResult::Redirect(next));
    }

    if response.status() == StatusCode::NOT_FOUND {
        return Err(LinkPreviewResolverStatus::Unsupported);
    }
    if !response.status().is_success() {
        return Err(LinkPreviewResolverStatus::Failed);
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or(value)
                .trim()
                .to_ascii_lowercase()
        });
    if !matches!(
        content_type.as_deref(),
        Some("text/html") | Some("application/xhtml+xml")
    ) {
        return Err(LinkPreviewResolverStatus::Unsupported);
    }
    if let Some(len) = response.content_length() {
        if len as usize > policy.max_bytes {
            return Err(LinkPreviewResolverStatus::Failed);
        }
    }

    let mut body = Vec::with_capacity(policy.max_bytes.min(64 * 1024));
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| LinkPreviewResolverStatus::Failed)?
    {
        if body.len() + chunk.len() > policy.max_bytes {
            return Err(LinkPreviewResolverStatus::Failed);
        }
        body.extend_from_slice(&chunk);
    }
    let html = String::from_utf8_lossy(&body).into_owned();
    Ok(FetchOnceResult::Html {
        final_url: url.clone(),
        html,
    })
}

struct PreparedTarget<'a> {
    host: Host<&'a str>,
    addrs: Vec<SocketAddr>,
}

async fn prepare_target<'a>(
    url: &'a Url,
    policy: &LinkPreviewResolverPolicy,
) -> Result<PreparedTarget<'a>, LinkPreviewResolverStatus> {
    let status = classify_url_with_policy(url, policy);
    if status != LinkPreviewResolverStatus::Ready {
        return Err(status);
    }
    let host = url.host().ok_or(LinkPreviewResolverStatus::Unsupported)?;
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs = match host {
        Host::Ipv4(ip) => vec![SocketAddr::new(IpAddr::V4(ip), port)],
        Host::Ipv6(ip) => vec![SocketAddr::new(IpAddr::V6(ip), port)],
        Host::Domain(name) => {
            let ips = tokio::net::lookup_host((name, port))
                .await
                .map_err(|_| LinkPreviewResolverStatus::Failed)?
                .map(|addr| addr.ip())
                .collect::<Vec<_>>();
            if ips.is_empty() {
                return Err(LinkPreviewResolverStatus::Failed);
            }
            let test_loopback = policy.allow_http_loopback_for_tests
                && url.scheme() == "http"
                && ips.iter().all(|ip| ip.is_loopback());
            if !test_loopback && ips.iter().any(|ip| !is_global_ip(*ip)) {
                return Err(LinkPreviewResolverStatus::Blocked);
            }
            ips.into_iter()
                .map(|ip| SocketAddr::new(ip, port))
                .collect()
        }
    };
    Ok(PreparedTarget { host, addrs })
}

fn classify_url_with_policy(
    url: &Url,
    policy: &LinkPreviewResolverPolicy,
) -> LinkPreviewResolverStatus {
    if url.scheme() != "https" {
        if policy.allow_http_loopback_for_tests
            && url.scheme() == "http"
            && url.host().is_some_and(host_is_loopback)
        {
            return LinkPreviewResolverStatus::Ready;
        }
        return LinkPreviewResolverStatus::Unsupported;
    }
    if matches!(url.host(), Some(Host::Ipv4(_)) | Some(Host::Ipv6(_))) {
        return LinkPreviewResolverStatus::Blocked;
    }
    if let Some(Host::Domain(host)) = url.host() {
        if host.trim_end_matches('.').ends_with(".local") {
            return LinkPreviewResolverStatus::Blocked;
        }
    }
    LinkPreviewResolverStatus::Ready
}

fn host_is_loopback(host: Host<&str>) -> bool {
    match host {
        Host::Ipv4(ip) => ip.is_loopback(),
        Host::Ipv6(ip) => ip.is_loopback(),
        Host::Domain(host) => host.eq_ignore_ascii_case("localhost"),
    }
}

fn is_global_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_global_ipv4(ip),
        IpAddr::V6(ip) => is_global_ipv6(ip),
    }
}

fn is_global_ipv4(ip: Ipv4Addr) -> bool {
    if ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
    {
        return false;
    }
    let octets = ip.octets();
    if octets[0] == 0 {
        return false;
    }
    if octets[0] == 100 && (octets[1] & 0xc0) == 0x40 {
        return false;
    }
    if octets[0] == 198 && (octets[1] & 0xfe) == 18 {
        return false;
    }
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
    if let Some(v4) = ip.to_ipv4() {
        return is_global_ipv4(v4);
    }
    let segs = ip.segments();
    if (segs[0] & 0xffc0) == 0xfec0 {
        return false;
    }
    if segs[0] == 0x2001 && segs[1] == 0x0db8 {
        return false;
    }
    if segs[0] == 0x0100 && segs[1] == 0 && segs[2] == 0 && segs[3] == 0 {
        return false;
    }
    true
}

pub(super) fn extract_metadata_from_html(
    requested_url: &Url,
    html: &str,
) -> Option<ResolvedLinkMetadata> {
    let title = meta_content(html, "og:title");
    let description = meta_content(html, "og:description");
    let normalized_url = meta_content(html, "og:url")
        .and_then(|url| Url::parse(&url).ok())
        .unwrap_or_else(|| requested_url.clone());
    if title.is_none() && description.is_none() && normalized_url == *requested_url {
        return None;
    }

    Some(ResolvedLinkMetadata {
        original_url: requested_url.clone(),
        normalized_url,
        title,
        description,
    })
}

fn meta_content(html: &str, property: &str) -> Option<String> {
    let mut remaining = html;
    while let Some(start) = find_ascii_case_insensitive(remaining, "<meta") {
        remaining = &remaining[start + "<meta".len()..];
        let end = remaining.find('>')?;
        let tag = &remaining[..end];
        remaining = &remaining[end + 1..];

        let attrs = parse_tag_attrs(tag);
        let name_matches = attrs.iter().any(|(name, value)| {
            (name.eq_ignore_ascii_case("property") || name.eq_ignore_ascii_case("name"))
                && value.eq_ignore_ascii_case(property)
        });
        if !name_matches {
            continue;
        }
        if let Some((_, content)) = attrs
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content"))
        {
            let content = html_escape::decode_html_entities(content)
                .trim()
                .to_string();
            if !content.is_empty() {
                return Some(content);
            }
        }
    }
    None
}

fn parse_tag_attrs(tag: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let bytes = tag.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let name_start = i;
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b':' | b'_' | b'-'))
        {
            i += 1;
        }
        if i == name_start {
            i += 1;
            continue;
        }
        let name = &tag[name_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let value = if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            i += 1;
            let value_start = i;
            while i < bytes.len() && bytes[i] != quote {
                i += 1;
            }
            let value = &tag[value_start..i];
            if i < bytes.len() {
                i += 1;
            }
            value
        } else {
            let value_start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            &tag[value_start..i]
        };
        attrs.push((name.to_string(), value.to_string()));
    }
    attrs
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn extracts_opengraph_text_metadata_from_html_without_executing_embeds() {
        let requested_url =
            Url::parse("https://Example.COM:443/articles?id=42&utm=keep").expect("url");
        let html = r#"
            <!doctype html>
            <html>
              <head>
                <title>Fallback title</title>
                <meta property="og:title" content="The Best Webpage">
                <meta property="og:description" content="Plain text preview">
                <meta property="og:url" content="https://example.com/articles?id=42&utm=keep">
                <script>throw new Error("must not execute")</script>
                <iframe src="https://embed.example/"></iframe>
              </head>
            </html>
        "#;

        let metadata = extract_metadata_from_html(&requested_url, html).expect("metadata");

        assert_eq!(
            metadata.original_url.as_str(),
            "https://example.com/articles?id=42&utm=keep"
        );
        assert_eq!(
            metadata.normalized_url.as_str(),
            "https://example.com/articles?id=42&utm=keep"
        );
        assert_eq!(metadata.title.as_deref(), Some("The Best Webpage"));
        assert_eq!(metadata.description.as_deref(), Some("Plain text preview"));
    }

    #[test]
    fn html_without_usable_metadata_is_unsupported() {
        let requested_url = Url::parse("https://example.com/articles").expect("url");

        assert!(extract_metadata_from_html(&requested_url, "<html><head></head></html>").is_none());
    }

    #[test]
    fn classifies_non_https_urls_as_unsupported_normal_outcomes() {
        let url = Url::parse("http://example.com/article").expect("url");

        assert_eq!(
            classify_url_with_policy(&url, &LinkPreviewResolverPolicy::default()),
            LinkPreviewResolverStatus::Unsupported
        );
    }

    #[test]
    fn blocks_ip_literal_targets_by_default() {
        for raw in [
            "https://127.0.0.1/article",
            "https://10.0.0.1/article",
            "https://169.254.169.254/latest/meta-data/",
            "https://224.0.0.1/article",
            "https://93.184.216.34/article",
            "https://[::1]/article",
            "https://[fc00::1]/article",
        ] {
            let url = Url::parse(raw).expect("url");
            assert_eq!(
                classify_url_with_policy(&url, &LinkPreviewResolverPolicy::default()),
                LinkPreviewResolverStatus::Blocked,
                "{raw}"
            );
        }
    }

    #[test]
    fn blocks_dot_local_domains_before_fetch() {
        let url = Url::parse("https://printer.local/article").expect("url");

        assert_eq!(
            classify_url_with_policy(&url, &LinkPreviewResolverPolicy::default()),
            LinkPreviewResolverStatus::Blocked
        );
    }

    #[tokio::test]
    async fn fetches_html_metadata_with_bounded_test_policy() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/article"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"<html><head>
                          <meta property="og:title" content="Fetched title">
                          <meta property="og:description" content="Fetched description">
                          <meta property="og:url" content="http://127.0.0.1/article">
                        </head></html>"#,
                "text/html; charset=utf-8",
            ))
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/article", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        assert_eq!(metadata.title.as_deref(), Some("Fetched title"));
        assert_eq!(metadata.description.as_deref(), Some("Fetched description"));
    }

    #[tokio::test]
    async fn oversized_html_response_returns_failed_outcome() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/huge"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("<html>".repeat(64), "text/html"))
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 64,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/huge", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Failed);
    }

    #[tokio::test]
    async fn redirect_into_blocked_target_returns_blocked_without_following() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/redirect"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "https://127.0.0.1/admin"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/redirect", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Blocked);
    }

    #[tokio::test]
    async fn redirect_count_above_policy_cap_returns_failed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/one"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/two"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/two"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/three"))
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_redirects: 1,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/one", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Failed);
    }

    #[tokio::test]
    async fn fetch_duration_above_policy_timeout_returns_failed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/slow"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(100))
                    .set_body_raw("<html></html>", "text/html"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            timeout: Duration::from_millis(10),
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/slow", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Failed);
    }
}
