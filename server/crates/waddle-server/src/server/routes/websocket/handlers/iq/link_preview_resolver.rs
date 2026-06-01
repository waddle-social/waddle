use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use reqwest::header::{CONTENT_TYPE, LOCATION};
use reqwest::{redirect, Client, StatusCode};
use sha2::{Digest, Sha256};
use tracing::warn;
use url::{Host, Url};

use crate::storage::BlobStorage;

const DEFAULT_MAX_BYTES: usize = 256 * 1024;
const DEFAULT_MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
const DEFAULT_MAX_REDIRECTS: usize = 3;
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1_500);
const LINK_PREVIEW_TITLE_MAX_BYTES: usize = 256;
const LINK_PREVIEW_DESCRIPTION_MAX_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedLinkMetadata {
    pub original_url: Url,
    pub normalized_url: Url,
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<ResolvedLinkPreviewImage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedLinkPreviewImage {
    pub url: Url,
    pub media_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub alt: Option<String>,
}

#[derive(Clone)]
pub(super) struct LinkPreviewMediaCache {
    storage: Arc<dyn BlobStorage>,
    public_base_url: String,
}

impl LinkPreviewMediaCache {
    pub(super) fn new(storage: Arc<dyn BlobStorage>, public_base_url: impl Into<String>) -> Self {
        Self {
            storage,
            public_base_url: public_base_url.into().trim_end_matches('/').to_string(),
        }
    }
}

#[derive(Clone)]
pub(super) struct LinkPreviewResolverPolicy {
    pub max_bytes: usize,
    pub max_image_bytes: usize,
    pub max_redirects: usize,
    pub timeout: Duration,
    pub allow_http_loopback_for_tests: bool,
    pub media_cache: Option<LinkPreviewMediaCache>,
}

impl Default for LinkPreviewResolverPolicy {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            timeout: DEFAULT_TIMEOUT,
            allow_http_loopback_for_tests: false,
            media_cache: None,
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
    let deadline = Instant::now() + policy.timeout;
    for redirect_count in 0..=policy.max_redirects {
        let Some(timeout) = deadline.checked_duration_since(Instant::now()) else {
            return LinkPreviewResolverOutcome::Failed;
        };
        let fetch = match fetch_html_once(&current, policy, timeout).await {
            Ok(fetch) => fetch,
            Err(LinkPreviewResolverStatus::Blocked) => return LinkPreviewResolverOutcome::Blocked,
            Err(LinkPreviewResolverStatus::Failed) => return LinkPreviewResolverOutcome::Failed,
            Err(LinkPreviewResolverStatus::Unsupported) => {
                return LinkPreviewResolverOutcome::Unsupported;
            }
            Err(LinkPreviewResolverStatus::Ready) => unreachable!("ready is not a fetch error"),
        };
        match fetch {
            FetchOnceResult::Html { final_url, html } => {
                let Some((mut metadata, remote_image)) =
                    extract_metadata_parts_from_html(url, &html, &final_url, policy)
                else {
                    return LinkPreviewResolverOutcome::Unsupported;
                };
                if let (Some(cache), Some(remote_image)) =
                    (policy.media_cache.as_ref(), remote_image)
                {
                    match fetch_cached_preview_image(&remote_image, policy, cache, deadline).await {
                        Ok(image) => metadata.image = Some(image),
                        Err(LinkPreviewResolverStatus::Blocked) => warn!(
                            url = %remote_image.url,
                            "dropping blocked link preview image after metadata resolved"
                        ),
                        Err(LinkPreviewResolverStatus::Failed) => warn!(
                            url = %remote_image.url,
                            "dropping failed link preview image after metadata resolved"
                        ),
                        Err(LinkPreviewResolverStatus::Unsupported) => {}
                        Err(LinkPreviewResolverStatus::Ready) => {
                            unreachable!("ready is not an image fetch error")
                        }
                    }
                }
                return LinkPreviewResolverOutcome::Ready(Box::new(metadata));
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

async fn fetch_cached_preview_image(
    remote: &RemotePreviewImage,
    policy: &LinkPreviewResolverPolicy,
    cache: &LinkPreviewMediaCache,
    deadline: Instant,
) -> Result<ResolvedLinkPreviewImage, LinkPreviewResolverStatus> {
    let mut current = remote.url.clone();
    for redirect_count in 0..=policy.max_redirects {
        let Some(timeout) = deadline.checked_duration_since(Instant::now()) else {
            return Err(LinkPreviewResolverStatus::Failed);
        };
        match fetch_image_once(&current, policy, timeout).await? {
            FetchImageOnceResult::Image { bytes, media_type } => {
                let digest = Sha256::digest(bytes.as_ref());
                let hash = digest
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                let key = format!("link-previews/sha256/{hash}");
                cache
                    .storage
                    .put(&key, bytes, &media_type)
                    .await
                    .map_err(|error| {
                        warn!(%error, key = %key, "failed to cache link preview image");
                        LinkPreviewResolverStatus::Unsupported
                    })?;
                let url = Url::parse(&format!(
                    "{}/api/link-preview-media/sha256/{hash}",
                    cache.public_base_url
                ))
                .map_err(|error| {
                    warn!(%error, "failed to build cached link preview image URL");
                    LinkPreviewResolverStatus::Unsupported
                })?;
                return Ok(ResolvedLinkPreviewImage {
                    url,
                    media_type,
                    width: remote.width,
                    height: remote.height,
                    alt: remote.alt.clone(),
                });
            }
            FetchImageOnceResult::Redirect(next) => {
                if redirect_count == policy.max_redirects {
                    return Err(LinkPreviewResolverStatus::Failed);
                }
                current = next;
            }
        }
    }
    Err(LinkPreviewResolverStatus::Failed)
}

enum FetchImageOnceResult {
    Image { bytes: Bytes, media_type: String },
    Redirect(Url),
}

async fn fetch_image_once(
    url: &Url,
    policy: &LinkPreviewResolverPolicy,
    timeout: Duration,
) -> Result<FetchImageOnceResult, LinkPreviewResolverStatus> {
    let target = prepare_target(url, policy).await?;
    let mut builder = Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
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
        return Ok(FetchImageOnceResult::Redirect(next));
    }

    if response.status() == StatusCode::NOT_FOUND {
        return Err(LinkPreviewResolverStatus::Unsupported);
    }
    if !response.status().is_success() {
        return Err(LinkPreviewResolverStatus::Failed);
    }
    let header_media_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(safe_preview_image_media_type)
        .ok_or(LinkPreviewResolverStatus::Unsupported)?;
    if let Some(len) = response.content_length() {
        if len > policy.max_image_bytes as u64 {
            return Err(LinkPreviewResolverStatus::Failed);
        }
    }
    let mut body = Vec::with_capacity(policy.max_image_bytes.min(128 * 1024));
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| LinkPreviewResolverStatus::Failed)?
    {
        if body.len() + chunk.len() > policy.max_image_bytes {
            return Err(LinkPreviewResolverStatus::Failed);
        }
        body.extend_from_slice(&chunk);
    }
    let media_type =
        sniff_safe_preview_image_media_type(&body).ok_or(LinkPreviewResolverStatus::Unsupported)?;
    if header_media_type != media_type {
        return Err(LinkPreviewResolverStatus::Unsupported);
    }
    Ok(FetchImageOnceResult::Image {
        bytes: Bytes::from(body),
        media_type,
    })
}

enum FetchOnceResult {
    Html { final_url: Url, html: String },
    Redirect(Url),
}

async fn fetch_html_once(
    url: &Url,
    policy: &LinkPreviewResolverPolicy,
    timeout: Duration,
) -> Result<FetchOnceResult, LinkPreviewResolverStatus> {
    let target = prepare_target(url, policy).await?;
    // The client is per-hop because each redirect target needs a fresh DNS pin;
    // max_redirects keeps the extra TLS/client setup cost bounded.
    let mut builder = Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .redirect(redirect::Policy::none())
        .https_only(!policy.allow_http_loopback_for_tests);
    if let Host::Domain(host) = target.host {
        // SSRF defense relies on reqwest dialing only the DNS results validated in
        // prepare_target; dropping this pin would let reqwest re-resolve the host.
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
        if len > policy.max_bytes as u64 {
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
        if is_dot_local_domain(host) {
            return LinkPreviewResolverStatus::Blocked;
        }
    }
    LinkPreviewResolverStatus::Ready
}

fn is_dot_local_domain(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    host.eq_ignore_ascii_case("local") || host.to_ascii_lowercase().ends_with(".local")
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
    if let Some(v4) = ip.to_ipv4_mapped().or_else(|| ip.to_ipv4()) {
        return is_global_ipv4(v4);
    }
    let segs = ip.segments();
    if segs[0] == 0x2002 {
        return is_global_ipv4(Ipv4Addr::new(
            (segs[1] >> 8) as u8,
            segs[1] as u8,
            (segs[2] >> 8) as u8,
            segs[2] as u8,
        ));
    }
    if segs[0] == 0x0064
        && segs[1] == 0xff9b
        && segs[2] == 0
        && segs[3] == 0
        && segs[4] == 0
        && segs[5] == 0
    {
        return is_global_ipv4(Ipv4Addr::new(
            (segs[6] >> 8) as u8,
            segs[6] as u8,
            (segs[7] >> 8) as u8,
            segs[7] as u8,
        ));
    }
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

#[cfg(test)]
fn extract_metadata_from_html(requested_url: &Url, html: &str) -> Option<ResolvedLinkMetadata> {
    extract_metadata_from_html_with_normalized_fallback(requested_url, html, requested_url)
}

#[cfg(test)]
fn extract_metadata_from_html_with_normalized_fallback(
    requested_url: &Url,
    html: &str,
    normalized_fallback_url: &Url,
) -> Option<ResolvedLinkMetadata> {
    let policy = LinkPreviewResolverPolicy::default();
    extract_metadata_parts_from_html(requested_url, html, normalized_fallback_url, &policy)
        .map(|(metadata, _)| metadata)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemotePreviewImage {
    url: Url,
    width: Option<u32>,
    height: Option<u32>,
    alt: Option<String>,
}

fn extract_metadata_parts_from_html(
    requested_url: &Url,
    html: &str,
    normalized_fallback_url: &Url,
    policy: &LinkPreviewResolverPolicy,
) -> Option<(ResolvedLinkMetadata, Option<RemotePreviewImage>)> {
    let title = meta_content(html, "og:title", LINK_PREVIEW_TITLE_MAX_BYTES);
    let description = meta_content(html, "og:description", LINK_PREVIEW_DESCRIPTION_MAX_BYTES);
    let canonical_url = meta_content(html, "og:url", usize::MAX)
        .and_then(|url| Url::parse(&url).ok())
        .filter(|url| {
            classify_url_with_policy(url, policy) == LinkPreviewResolverStatus::Ready
                && same_domain_host(url, normalized_fallback_url)
        });
    let normalized_url = canonical_url
        .clone()
        .unwrap_or_else(|| normalized_fallback_url.clone());
    if title.is_none()
        && description.is_none()
        && canonical_url.is_none()
        && normalized_url == *requested_url
    {
        return None;
    }

    let image = meta_content(html, "og:image", usize::MAX)
        .and_then(|url| Url::parse(&url).ok())
        .filter(|url| classify_url_with_policy(url, policy) == LinkPreviewResolverStatus::Ready)
        .map(|url| RemotePreviewImage {
            url,
            width: meta_content(html, "og:image:width", 16).and_then(|raw| raw.parse().ok()),
            height: meta_content(html, "og:image:height", 16).and_then(|raw| raw.parse().ok()),
            alt: meta_content(html, "og:image:alt", LINK_PREVIEW_DESCRIPTION_MAX_BYTES),
        });

    Some((
        ResolvedLinkMetadata {
            original_url: requested_url.clone(),
            normalized_url,
            title,
            description,
            image: None,
        },
        image,
    ))
}

fn safe_preview_image_media_type(value: &str) -> Option<String> {
    let media_type = value
        .split(';')
        .next()
        .unwrap_or(value)
        .trim()
        .to_ascii_lowercase();
    matches!(
        media_type.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
    .then_some(media_type)
}

fn sniff_safe_preview_image_media_type(bytes: &[u8]) -> Option<String> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png".to_string());
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg".to_string());
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif".to_string());
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp".to_string());
    }
    None
}

fn meta_content(html: &str, property: &str, max_bytes: usize) -> Option<String> {
    let mut remaining = html;
    while let Some(start) = find_meta_tag_start(remaining) {
        remaining = &remaining[start + "<meta".len()..];
        let Some(end) = find_meta_tag_end(remaining) else {
            break;
        };
        if let Some(next_start) = find_meta_tag_start(remaining) {
            if next_start < end {
                remaining = &remaining[next_start..];
                continue;
            }
        }
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
                return Some(truncate_utf8_to_bytes(&content, max_bytes));
            }
        }
    }
    None
}

fn same_domain_host(left: &Url, right: &Url) -> bool {
    match (left.host(), right.host()) {
        (Some(Host::Domain(left)), Some(Host::Domain(right))) => left
            .trim_end_matches('.')
            .eq_ignore_ascii_case(right.trim_end_matches('.')),
        _ => false,
    }
}

fn find_meta_tag_start(html: &str) -> Option<usize> {
    let mut offset = 0;
    while let Some(start) = find_ascii_case_insensitive(&html[offset..], "<meta") {
        let absolute_start = offset + start;
        let after_name = absolute_start + "<meta".len();
        match html.as_bytes().get(after_name) {
            None | Some(b'>') | Some(b'/') => return Some(absolute_start),
            Some(byte) if byte.is_ascii_whitespace() => return Some(absolute_start),
            _ => offset = after_name,
        }
    }
    None
}

fn find_meta_tag_end(tag_tail: &str) -> Option<usize> {
    let mut quote = None;
    for (index, byte) in tag_tail.bytes().enumerate() {
        match (quote, byte) {
            (Some(open), current) if current == open => quote = None,
            (None, b'"' | b'\'') => quote = Some(byte),
            (None, b'>') => return Some(index),
            _ => {}
        }
    }
    None
}

fn truncate_utf8_to_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
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
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct FailingPutStorage;

    impl crate::storage::BlobStorage for FailingPutStorage {
        fn put(
            &self,
            _key: &str,
            _data: bytes::Bytes,
            _content_type: &str,
        ) -> Pin<Box<dyn Future<Output = Result<(), crate::storage::StorageError>> + Send + '_>>
        {
            Box::pin(async {
                Err(crate::storage::StorageError::Internal(
                    "forced put failure".to_string(),
                ))
            })
        }

        fn get(
            &self,
            key: &str,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            (bytes::Bytes, crate::storage::BlobMeta),
                            crate::storage::StorageError,
                        >,
                    > + Send
                    + '_,
            >,
        > {
            let key = key.to_string();
            Box::pin(async { Err(crate::storage::StorageError::NotFound(key)) })
        }
    }

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
    fn malformed_meta_tag_does_not_abort_later_metadata_extraction() {
        let requested_url = Url::parse("https://example.com/articles").expect("url");
        let html = r#"<html><head>
                <meta property="og:title" content="Broken"
                <meta property="og:title" content="Recovered title">
                <meta property="og:description" content="Recovered description">
              </head></html>"#;

        let metadata = extract_metadata_from_html(&requested_url, html).expect("metadata");

        assert_eq!(metadata.title.as_deref(), Some("Recovered title"));
        assert_eq!(
            metadata.description.as_deref(),
            Some("Recovered description")
        );
    }

    #[test]
    fn meta_tag_terminator_ignores_greater_than_inside_quoted_attribute_values() {
        let requested_url = Url::parse("https://example.com/articles").expect("url");
        let html = r#"<meta property="og:title" content="A&gt;B">"#;

        let metadata = extract_metadata_from_html(&requested_url, html).expect("metadata");

        assert_eq!(metadata.title.as_deref(), Some("A>B"));
    }

    #[test]
    fn scanner_ignores_non_meta_tag_names_with_meta_prefix() {
        let requested_url = Url::parse("https://example.com/articles").expect("url");
        let html = r#"<metadata property="og:title" content="Wrong">
            <meta property="og:title" content="Right">"#;

        let metadata = extract_metadata_from_html(&requested_url, html).expect("metadata");

        assert_eq!(metadata.title.as_deref(), Some("Right"));
    }

    #[test]
    fn ignores_blocked_open_graph_canonical_urls() {
        let requested_url = Url::parse("https://example.com/articles").expect("url");
        let html = r#"<html><head>
                <meta property="og:title" content="Safe title">
                <meta property="og:url" content="https://Printer.Local/admin">
              </head></html>"#;

        let metadata = extract_metadata_from_html(&requested_url, html).expect("metadata");

        assert_eq!(metadata.normalized_url, requested_url);
    }

    #[test]
    fn ignores_cross_host_open_graph_canonical_urls() {
        let requested_url = Url::parse("https://attacker.example/articles").expect("url");
        let final_url = Url::parse("https://attacker.example/final").expect("url");
        let html = r#"<html><head>
                <meta property="og:title" content="Safe title">
                <meta property="og:url" content="https://bank.example/login">
              </head></html>"#;

        let metadata =
            extract_metadata_from_html_with_normalized_fallback(&requested_url, html, &final_url)
                .expect("metadata");

        assert_eq!(metadata.normalized_url, final_url);
    }

    #[test]
    fn preserves_safe_open_graph_canonical_url_over_fetch_url() {
        let requested_url = Url::parse("https://example.com/articles?utm=keep").expect("url");
        let final_url = Url::parse("https://example.com/redirected?utm=keep").expect("url");
        let canonical_url = Url::parse("https://example.com/articles").expect("url");
        let html = r#"<html><head>
                <meta property="og:title" content="Safe title">
                <meta property="og:url" content="https://example.com/articles">
              </head></html>"#;

        let metadata =
            extract_metadata_from_html_with_normalized_fallback(&requested_url, html, &final_url)
                .expect("metadata");

        assert_eq!(metadata.normalized_url, canonical_url);
    }

    #[test]
    fn falls_back_to_fetch_url_when_open_graph_canonical_url_is_absent() {
        let requested_url = Url::parse("https://example.com/articles?utm=keep").expect("url");
        let final_url = Url::parse("https://example.com/redirected").expect("url");
        let html = r#"<html><head>
                <meta property="og:title" content="Safe title">
              </head></html>"#;

        let metadata =
            extract_metadata_from_html_with_normalized_fallback(&requested_url, html, &final_url)
                .expect("metadata");

        assert_eq!(metadata.normalized_url, final_url);
    }

    #[test]
    fn truncates_decoded_text_metadata_to_field_byte_limits() {
        let requested_url = Url::parse("https://example.com/articles").expect("url");
        let title = format!("{}&eacute;", "t".repeat(LINK_PREVIEW_TITLE_MAX_BYTES - 1));
        let description = format!(
            "{}&eacute;",
            "d".repeat(LINK_PREVIEW_DESCRIPTION_MAX_BYTES - 1)
        );
        let html = format!(
            r#"<html><head>
                <meta property="og:title" content="{title}">
                <meta property="og:description" content="{description}">
              </head></html>"#
        );

        let metadata = extract_metadata_from_html(&requested_url, &html).expect("metadata");

        assert_eq!(
            metadata.title.as_deref().map(str::len),
            Some(LINK_PREVIEW_TITLE_MAX_BYTES - 1)
        );
        assert_eq!(
            metadata.description.as_deref().map(str::len),
            Some(LINK_PREVIEW_DESCRIPTION_MAX_BYTES - 1)
        );
        assert_eq!(
            metadata.title.as_deref(),
            Some("t".repeat(LINK_PREVIEW_TITLE_MAX_BYTES - 1).as_str())
        );
        assert_eq!(
            metadata.description.as_deref(),
            Some("d".repeat(LINK_PREVIEW_DESCRIPTION_MAX_BYTES - 1).as_str())
        );
    }

    #[test]
    fn text_metadata_truncation_preserves_utf8_boundaries() {
        let requested_url = Url::parse("https://example.com/articles").expect("url");
        let title = "é".repeat(LINK_PREVIEW_TITLE_MAX_BYTES);
        let html = format!(r#"<meta property="og:title" content="{title}">"#);

        let metadata = extract_metadata_from_html(&requested_url, &html).expect("metadata");

        assert!(metadata.title.expect("title").len() <= LINK_PREVIEW_TITLE_MAX_BYTES);
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
            "https://[2002:0a00:0001::1]/article",
            "https://[64:ff9b::0a00:0001]/article",
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
        for raw in [
            "https://printer.local/article",
            "https://Printer.Local/article",
            "https://printer.local./article",
        ] {
            let url = Url::parse(raw).expect("url");

            assert_eq!(
                classify_url_with_policy(&url, &LinkPreviewResolverPolicy::default()),
                LinkPreviewResolverStatus::Blocked,
                "{raw}"
            );
        }
        assert!(is_dot_local_domain("Printer.Local"));
        assert!(is_dot_local_domain("Printer.Local."));
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
    async fn fetches_safe_preview_image_into_content_addressed_waddle_storage() {
        let server = MockServer::start().await;
        let image_bytes = bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\nfake png bytes");
        Mock::given(method("GET"))
            .and(path("/article"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"<html><head>
                          <meta property="og:title" content="Fetched title">
                          <meta property="og:image" content="{}/preview.png">
                          <meta property="og:image:width" content="640">
                          <meta property="og:image:height" content="360">
                          <meta property="og:image:alt" content="Article screenshot">
                        </head></html>"#,
                    server.uri()
                ),
                "text/html; charset=utf-8",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/preview.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(image_bytes.clone()),
            )
            .mount(&server)
            .await;
        let storage_dir =
            std::env::temp_dir().join(format!("waddle-link-preview-{}", uuid::Uuid::new_v4()));
        let storage: Arc<dyn crate::storage::BlobStorage> =
            Arc::new(crate::storage::LocalStorage::new(storage_dir));
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            media_cache: Some(LinkPreviewMediaCache::new(
                storage.clone(),
                "https://waddle.example",
            )),
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/article", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        let expected_hash = hex::encode(Sha256::digest(image_bytes.as_ref()));
        let expected_key = format!("link-previews/sha256/{expected_hash}");
        let image = metadata.image.expect("cached image metadata");
        assert_eq!(image.url.scheme(), "https");
        assert_eq!(image.url.domain(), Some("waddle.example"));
        assert_eq!(
            image.url.path(),
            format!("/api/link-preview-media/sha256/{expected_hash}")
        );
        assert_eq!(image.media_type, "image/png");
        assert_eq!(image.width, Some(640));
        assert_eq!(image.height, Some(360));
        assert_eq!(image.alt.as_deref(), Some("Article screenshot"));
        let (stored, meta) = storage.get(&expected_key).await.expect("cached bytes");
        assert_eq!(stored, image_bytes);
        assert_eq!(meta.content_type, "image/png");
    }

    #[tokio::test]
    async fn rejects_unsafe_preview_image_media_types_without_caching_image() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/article"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"<html><head>
                          <meta property="og:title" content="Fetched title">
                          <meta property="og:image" content="{}/preview.svg">
                        </head></html>"#,
                    server.uri()
                ),
                "text/html; charset=utf-8",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/preview.svg"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/svg+xml")
                    .set_body_raw("<svg/>", "image/svg+xml"),
            )
            .mount(&server)
            .await;
        let storage_dir =
            std::env::temp_dir().join(format!("waddle-link-preview-{}", uuid::Uuid::new_v4()));
        let storage: Arc<dyn crate::storage::BlobStorage> =
            Arc::new(crate::storage::LocalStorage::new(storage_dir));
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            media_cache: Some(LinkPreviewMediaCache::new(
                storage.clone(),
                "https://waddle.example",
            )),
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/article", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        assert_eq!(metadata.title.as_deref(), Some("Fetched title"));
        assert_eq!(metadata.image, None);
        assert!(storage
            .get("link-previews/sha256/d4dc56669143034f31aa309635d4113d9ad76a02b1739da22c965ed2049be9e6")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn follows_safe_preview_image_redirects_before_caching() {
        let server = MockServer::start().await;
        let image_bytes = bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\nredirected png");
        Mock::given(method("GET"))
            .and(path("/article"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"<html><head>
                          <meta property="og:title" content="Fetched title">
                          <meta property="og:image" content="{}/preview">
                        </head></html>"#,
                    server.uri()
                ),
                "text/html; charset=utf-8",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/preview"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/final.png", server.uri())),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/final.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(image_bytes.clone()),
            )
            .mount(&server)
            .await;
        let storage_dir =
            std::env::temp_dir().join(format!("waddle-link-preview-{}", uuid::Uuid::new_v4()));
        let storage: Arc<dyn crate::storage::BlobStorage> =
            Arc::new(crate::storage::LocalStorage::new(storage_dir));
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            media_cache: Some(LinkPreviewMediaCache::new(
                storage.clone(),
                "https://waddle.example",
            )),
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/article", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        let expected_hash = hex::encode(Sha256::digest(image_bytes.as_ref()));
        assert_eq!(
            metadata.image.expect("cached image").url.path(),
            format!("/api/link-preview-media/sha256/{expected_hash}")
        );
    }

    #[tokio::test]
    async fn missing_preview_image_degrades_to_text_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/article"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"<html><head>
                          <meta property="og:title" content="Fetched title">
                          <meta property="og:image" content="{}/missing.png">
                        </head></html>"#,
                    server.uri()
                ),
                "text/html; charset=utf-8",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/missing.png"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let storage_dir =
            std::env::temp_dir().join(format!("waddle-link-preview-{}", uuid::Uuid::new_v4()));
        let storage: Arc<dyn crate::storage::BlobStorage> =
            Arc::new(crate::storage::LocalStorage::new(storage_dir));
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            media_cache: Some(LinkPreviewMediaCache::new(
                storage,
                "https://waddle.example",
            )),
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/article", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        assert_eq!(metadata.title.as_deref(), Some("Fetched title"));
        assert_eq!(metadata.image, None);
    }

    #[tokio::test]
    async fn mismatched_preview_image_header_and_bytes_degrades_to_text_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/article"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"<html><head>
                          <meta property="og:title" content="Fetched title">
                          <meta property="og:image" content="{}/preview.png">
                        </head></html>"#,
                    server.uri()
                ),
                "text/html; charset=utf-8",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/preview.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(bytes::Bytes::from_static(b"\xff\xd8\xffjpeg")),
            )
            .mount(&server)
            .await;
        let storage_dir =
            std::env::temp_dir().join(format!("waddle-link-preview-{}", uuid::Uuid::new_v4()));
        let storage: Arc<dyn crate::storage::BlobStorage> =
            Arc::new(crate::storage::LocalStorage::new(storage_dir));
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            media_cache: Some(LinkPreviewMediaCache::new(
                storage,
                "https://waddle.example",
            )),
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/article", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        assert_eq!(metadata.title.as_deref(), Some("Fetched title"));
        assert_eq!(metadata.image, None);
    }

    #[tokio::test]
    async fn cached_image_url_construction_failure_degrades_to_text_metadata() {
        let server = MockServer::start().await;
        let image_bytes = bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\nvalid png");
        Mock::given(method("GET"))
            .and(path("/article"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"<html><head>
                          <meta property="og:title" content="Fetched title">
                          <meta property="og:image" content="{}/preview.png">
                        </head></html>"#,
                    server.uri()
                ),
                "text/html; charset=utf-8",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/preview.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(image_bytes),
            )
            .mount(&server)
            .await;
        let storage_dir =
            std::env::temp_dir().join(format!("waddle-link-preview-{}", uuid::Uuid::new_v4()));
        let storage: Arc<dyn crate::storage::BlobStorage> =
            Arc::new(crate::storage::LocalStorage::new(storage_dir));
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            media_cache: Some(LinkPreviewMediaCache::new(storage, "not a url")),
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/article", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        assert_eq!(metadata.title.as_deref(), Some("Fetched title"));
        assert_eq!(metadata.image, None);
    }

    #[tokio::test]
    async fn cached_image_storage_failure_degrades_to_text_metadata() {
        let server = MockServer::start().await;
        let image_bytes = bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\nvalid png");
        Mock::given(method("GET"))
            .and(path("/article"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"<html><head>
                          <meta property="og:title" content="Fetched title">
                          <meta property="og:image" content="{}/preview.png">
                        </head></html>"#,
                    server.uri()
                ),
                "text/html; charset=utf-8",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/preview.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(image_bytes),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            media_cache: Some(LinkPreviewMediaCache::new(
                Arc::new(FailingPutStorage),
                "https://waddle.example",
            )),
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/article", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        assert_eq!(metadata.title.as_deref(), Some("Fetched title"));
        assert_eq!(metadata.image, None);
    }

    #[tokio::test]
    async fn blocked_preview_image_redirect_degrades_to_text_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/article"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"<html><head>
                          <meta property="og:title" content="Fetched title">
                          <meta property="og:image" content="{}/preview">
                        </head></html>"#,
                    server.uri()
                ),
                "text/html; charset=utf-8",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/preview"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "https://127.0.0.1/admin"),
            )
            .mount(&server)
            .await;
        let storage_dir =
            std::env::temp_dir().join(format!("waddle-link-preview-{}", uuid::Uuid::new_v4()));
        let storage: Arc<dyn crate::storage::BlobStorage> =
            Arc::new(crate::storage::LocalStorage::new(storage_dir));
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            media_cache: Some(LinkPreviewMediaCache::new(
                storage,
                "https://waddle.example",
            )),
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/article", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        assert_eq!(metadata.title.as_deref(), Some("Fetched title"));
        assert_eq!(metadata.image, None);
    }

    #[tokio::test]
    async fn failed_preview_image_response_degrades_to_text_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/article"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"<html><head>
                          <meta property="og:title" content="Fetched title">
                          <meta property="og:image" content="{}/preview.png">
                        </head></html>"#,
                    server.uri()
                ),
                "text/html; charset=utf-8",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/preview.png"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let storage_dir =
            std::env::temp_dir().join(format!("waddle-link-preview-{}", uuid::Uuid::new_v4()));
        let storage: Arc<dyn crate::storage::BlobStorage> =
            Arc::new(crate::storage::LocalStorage::new(storage_dir));
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            media_cache: Some(LinkPreviewMediaCache::new(
                storage,
                "https://waddle.example",
            )),
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/article", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        assert_eq!(metadata.title.as_deref(), Some("Fetched title"));
        assert_eq!(metadata.image, None);
    }

    #[tokio::test]
    async fn timed_out_preview_image_degrades_to_text_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/article"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"<html><head>
                          <meta property="og:title" content="Fetched title">
                          <meta property="og:image" content="{}/preview.png">
                        </head></html>"#,
                    server.uri()
                ),
                "text/html; charset=utf-8",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/preview.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(100))
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\nslow png")),
            )
            .mount(&server)
            .await;
        let storage_dir =
            std::env::temp_dir().join(format!("waddle-link-preview-{}", uuid::Uuid::new_v4()));
        let storage: Arc<dyn crate::storage::BlobStorage> =
            Arc::new(crate::storage::LocalStorage::new(storage_dir));
        let policy = LinkPreviewResolverPolicy {
            timeout: Duration::from_millis(20),
            allow_http_loopback_for_tests: true,
            media_cache: Some(LinkPreviewMediaCache::new(
                storage,
                "https://waddle.example",
            )),
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/article", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        assert_eq!(metadata.title.as_deref(), Some("Fetched title"));
        assert_eq!(metadata.image, None);
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
    async fn oversized_content_length_returns_failed_before_streaming() {
        let server = MockServer::start().await;
        let body = "x".repeat(65);
        Mock::given(method("GET"))
            .and(path("/huge-header"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .insert_header("content-length", body.len().to_string())
                    .set_body_raw(body, "text/html"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 64,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/huge-header", server.uri())).expect("url");

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
