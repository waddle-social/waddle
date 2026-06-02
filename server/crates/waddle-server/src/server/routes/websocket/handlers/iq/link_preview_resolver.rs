use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use chrono::Utc;
use jid::BareJid;
use kameo::actor::ActorRef;
use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE, LOCATION, RANGE};
use reqwest::{redirect, Client, StatusCode};
use sha2::{Digest, Sha256};
use tracing::warn;
use url::{Host, Url};
use waddle_xmpp_core::{DirectVideoMediaType, PreviewImageMediaType};

use crate::config::{LinkPreviewConfig, LinkPreviewHostPattern};
use crate::db::actor::{DbActor, DbExecute};
use crate::db::Value;
use crate::server::routes::websocket::link_preview_telemetry::{
    record_link_preview_event, LinkPreviewTelemetryEvent,
};
use crate::storage::BlobStorage;
use crate::storage::{BlobMeta, StorageError};

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
    /// Direct playable video discovered when the link itself is a trusted
    /// direct-media file. Mutually exclusive with HTML-derived previews.
    pub video: Option<ResolvedDirectVideo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedDirectVideo {
    /// Policy-validated remote URL the client plays from on user action.
    pub url: Url,
    pub media_type: DirectVideoMediaType,
    /// Total byte size when advertised by the origin.
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedLinkPreviewImage {
    pub url: Url,
    pub media_type: PreviewImageMediaType,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub alt: Option<String>,
}

#[derive(Clone)]
pub(super) struct LinkPreviewMediaCache {
    storage: Arc<dyn BlobStorage>,
    public_base_url: String,
    global_db_actor: ActorRef<DbActor>,
    requester_jid: BareJid,
}

impl LinkPreviewMediaCache {
    pub(super) fn new(
        storage: Arc<dyn BlobStorage>,
        public_base_url: impl Into<String>,
        global_db_actor: ActorRef<DbActor>,
        requester_jid: BareJid,
    ) -> Self {
        Self {
            storage,
            public_base_url: public_base_url.into().trim_end_matches('/').to_string(),
            global_db_actor,
            requester_jid,
        }
    }
}

#[derive(Clone)]
pub(super) struct LinkPreviewResolverPolicy {
    pub enabled: bool,
    pub allowed_hosts: Vec<LinkPreviewHostPattern>,
    pub blocked_hosts: Vec<LinkPreviewHostPattern>,
    pub max_bytes: usize,
    pub max_image_bytes: usize,
    pub max_redirects: usize,
    pub timeout: Duration,
    pub allow_http_loopback_for_tests: bool,
    pub media_cache: Option<LinkPreviewMediaCache>,
    pub video_enabled: bool,
}

impl Default for LinkPreviewResolverPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed_hosts: Vec::new(),
            blocked_hosts: Vec::new(),
            max_bytes: DEFAULT_MAX_BYTES,
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
            max_redirects: DEFAULT_MAX_REDIRECTS,
            timeout: DEFAULT_TIMEOUT,
            allow_http_loopback_for_tests: false,
            media_cache: None,
            video_enabled: true,
        }
    }
}

impl LinkPreviewResolverPolicy {
    pub(super) fn from_config(
        config: &LinkPreviewConfig,
        media_cache: Option<LinkPreviewMediaCache>,
    ) -> Self {
        Self {
            enabled: config.enabled,
            allowed_hosts: config.allowed_hosts.clone(),
            blocked_hosts: config.blocked_hosts.clone(),
            max_bytes: config.max_fetch_bytes,
            max_image_bytes: config.max_cached_image_bytes,
            max_redirects: config.max_redirects,
            timeout: config.fetch_timeout,
            allow_http_loopback_for_tests: false,
            media_cache,
            video_enabled: config.video_enabled,
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
            FetchOnceResult::DirectVideo {
                final_url,
                media_type,
                size,
            } => {
                return LinkPreviewResolverOutcome::Ready(Box::new(ResolvedLinkMetadata {
                    original_url: url.clone(),
                    normalized_url: final_url.clone(),
                    title: None,
                    description: None,
                    image: None,
                    video: Some(ResolvedDirectVideo {
                        url: final_url,
                        media_type,
                        size,
                    }),
                }));
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

fn looks_like_direct_video_url(url: &Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    matches!(
        path.rsplit('/').next(),
        Some(filename)
            if filename.ends_with(".mp4")
                || filename.ends_with(".webm")
                || filename.ends_with(".mov")
                || filename.ends_with(".m4v")
                || filename.ends_with(".ogv")
    )
}

/// Best-effort total byte size of a direct-media response.
///
/// A `Range` request can yield `206 Partial Content`, whose `Content-Length`
/// is only the returned slice; the authoritative total lives in the
/// `Content-Range` `…/<total>` suffix. Returns `None` when unknown.
fn direct_video_total_size(response: &reqwest::Response) -> Option<u64> {
    if response.status() == StatusCode::PARTIAL_CONTENT {
        response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit('/').next())
            .and_then(|total| total.trim().parse::<u64>().ok())
    } else {
        response.content_length()
    }
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
                let hash = hex::encode(Sha256::digest(bytes.as_ref()));
                let (slot_id, filename) =
                    publish_cached_preview_image_slot(cache, &hash, bytes, media_type.as_str())
                        .await?;
                let url = Url::parse(&format!(
                    "{}/api/files/{}/{}",
                    cache.public_base_url, slot_id, filename
                ))
                .map_err(|error| {
                    warn!(%error, "failed to build XEP-0363 cached link preview image URL");
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

async fn publish_cached_preview_image_slot(
    cache: &LinkPreviewMediaCache,
    hash: &str,
    bytes: Bytes,
    media_type: &str,
) -> Result<(String, String), LinkPreviewResolverStatus> {
    let key = format!("link-previews/sha256/{hash}");
    match cache.storage.get(&key).await {
        Ok((cached_bytes, meta)) => {
            record_link_preview_event(LinkPreviewTelemetryEvent::CacheHit);
            let filename = format!(
                "link-preview-{hash}.{}",
                preview_image_extension(meta.content_type.as_str())
            );
            return record_cached_preview_image_slot(
                cache,
                &key,
                &filename,
                cached_bytes.len(),
                &meta,
            )
            .await;
        }
        Err(StorageError::NotFound(_)) => {
            record_link_preview_event(LinkPreviewTelemetryEvent::CacheMiss)
        }
        Err(StorageError::Internal(_)) => {
            return Err(LinkPreviewResolverStatus::Failed);
        }
    }
    cache
        .storage
        .put(&key, bytes.clone(), media_type)
        .await
        .map_err(|error| {
            warn!(%error, key = %key, "failed to cache link preview image");
            LinkPreviewResolverStatus::Unsupported
        })?;

    let filename = format!(
        "link-preview-{hash}.{}",
        preview_image_extension(media_type)
    );
    record_cached_preview_image_slot(
        cache,
        &key,
        &filename,
        bytes.len(),
        &BlobMeta {
            content_type: media_type.to_string(),
        },
    )
    .await
}

async fn record_cached_preview_image_slot(
    cache: &LinkPreviewMediaCache,
    key: &str,
    filename: &str,
    size: usize,
    meta: &BlobMeta,
) -> Result<(String, String), LinkPreviewResolverStatus> {
    let slot_id = uuid::Uuid::new_v4().to_string();
    let now = Utc::now();
    let size_bytes = i64::try_from(size).map_err(|_| LinkPreviewResolverStatus::Failed)?;
    cache
        .global_db_actor
        .ask(DbExecute {
            sql: "INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, status, storage_key, expires_at, uploaded_at) VALUES (?, ?, ?, ?, ?, 'uploaded', ?, ?, ?)".to_string(),
            params: vec![
                Value::from(slot_id.clone()),
                Value::from(cache.requester_jid.to_string()),
                Value::from(filename.to_string()),
                Value::from(size_bytes),
                Value::from(meta.content_type.clone()),
                Value::from(key.to_string()),
                Value::from((now + chrono::Duration::minutes(15)).to_rfc3339()),
                Value::from(now.to_rfc3339()),
            ],
        })
        .await
        .map_err(|error| {
            warn!(%error, "failed to record XEP-0363 link preview upload slot");
            LinkPreviewResolverStatus::Unsupported
        })?;
    Ok((slot_id, filename.to_string()))
}

fn preview_image_extension(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    }
}

enum FetchImageOnceResult {
    Image {
        bytes: Bytes,
        media_type: PreviewImageMediaType,
    },
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
    Html {
        final_url: Url,
        html: String,
    },
    DirectVideo {
        final_url: Url,
        media_type: DirectVideoMediaType,
        size: Option<u64>,
    },
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
    let mut request = client.get(url.clone());
    if policy.max_bytes > 0 {
        request = request.header(RANGE, format!("bytes=0-{}", policy.max_bytes - 1));
    }
    let mut response = request
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
    if let Some(media_type) = content_type
        .as_deref()
        .and_then(|value| value.parse::<DirectVideoMediaType>().ok())
    {
        // Direct video is accepted only when policy allows it AND the URL itself
        // names a direct playable file. This blocks provider endpoints that
        // return a video content-type for non-file (embed/watch) URLs, and is
        // defense-in-depth on top of the classify-time `video_enabled` gate.
        if policy.video_enabled && looks_like_direct_video_url(url) {
            return Ok(FetchOnceResult::DirectVideo {
                final_url: url.clone(),
                media_type,
                size: direct_video_total_size(&response),
            });
        }
        return Err(LinkPreviewResolverStatus::Unsupported);
    }
    if !matches!(
        content_type.as_deref(),
        Some("text/html") | Some("application/xhtml+xml")
    ) {
        return Err(LinkPreviewResolverStatus::Unsupported);
    }
    let allow_head_cutoff = matches!(content_type.as_deref(), Some("text/html"));
    if let Some(len) = response.content_length() {
        if len > policy.max_bytes as u64 && response.status() != StatusCode::PARTIAL_CONTENT {
            return Err(LinkPreviewResolverStatus::Failed);
        }
    }
    let partial_content_state = if response.status() == StatusCode::PARTIAL_CONTENT {
        response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .map(parse_content_range)
            .unwrap_or(ContentRangeState::Unknown)
    } else {
        ContentRangeState::NotPartial
    };
    if partial_content_state == ContentRangeState::Unknown {
        return Err(LinkPreviewResolverStatus::Failed);
    }
    let mut body = Vec::with_capacity(policy.max_bytes.min(64 * 1024));
    let mut head_end_scanner = HtmlHeadEndScanner::default();
    let mut found_head_end = None;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| LinkPreviewResolverStatus::Failed)?
    {
        if body.len() + chunk.len() > policy.max_bytes {
            let remaining = policy.max_bytes.saturating_sub(body.len());
            body.extend_from_slice(&chunk[..remaining]);
            if allow_head_cutoff
                && found_head_end.is_none()
                && response.status() != StatusCode::PARTIAL_CONTENT
            {
                found_head_end = head_end_scanner.scan(&body);
                if let Some(head_end) = found_head_end {
                    body.truncate(head_end);
                    break;
                }
            }
            return Err(LinkPreviewResolverStatus::Failed);
        }
        body.extend_from_slice(&chunk);
        if allow_head_cutoff && found_head_end.is_none() {
            found_head_end = head_end_scanner.scan(&body);
            if response.status() != StatusCode::PARTIAL_CONTENT {
                if let Some(head_end) = found_head_end {
                    body.truncate(head_end);
                    break;
                }
            }
        }
    }
    if let Some(expected_len) = partial_content_state.expected_len() {
        let received_len = u64::try_from(body.len()).unwrap_or(u64::MAX);
        if received_len != expected_len {
            return Err(LinkPreviewResolverStatus::Failed);
        }
    }
    if allow_head_cutoff
        && found_head_end.is_none()
        && matches!(
            partial_content_state,
            ContentRangeState::HasUnreturnedTail { .. } | ContentRangeState::UnknownTotal { .. }
        )
    {
        return Err(LinkPreviewResolverStatus::Failed);
    }
    if let Some(head_end) = found_head_end {
        body.truncate(head_end);
    }
    let html = String::from_utf8_lossy(&body).into_owned();
    Ok(FetchOnceResult::Html {
        final_url: url.clone(),
        html,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentRangeState {
    NotPartial,
    Complete { expected_len: u64 },
    HasUnreturnedTail { expected_len: u64 },
    UnknownTotal { expected_len: u64 },
    Unknown,
}

impl ContentRangeState {
    fn expected_len(self) -> Option<u64> {
        match self {
            ContentRangeState::Complete { expected_len }
            | ContentRangeState::HasUnreturnedTail { expected_len }
            | ContentRangeState::UnknownTotal { expected_len } => Some(expected_len),
            ContentRangeState::NotPartial | ContentRangeState::Unknown => None,
        }
    }
}

fn parse_content_range(value: &str) -> ContentRangeState {
    let mut parts = value.split_whitespace();
    let Some(unit) = parts.next() else {
        return ContentRangeState::Unknown;
    };
    let Some(range_and_total) = parts.next() else {
        return ContentRangeState::Unknown;
    };
    if parts.next().is_some() || !unit.eq_ignore_ascii_case("bytes") {
        return ContentRangeState::Unknown;
    }
    let Some((range, total)) = range_and_total.split_once('/') else {
        return ContentRangeState::Unknown;
    };
    let Some((start, end)) = range.split_once('-') else {
        return ContentRangeState::Unknown;
    };
    let Ok(start) = start.parse::<u64>() else {
        return ContentRangeState::Unknown;
    };
    let Ok(end) = end.parse::<u64>() else {
        return ContentRangeState::Unknown;
    };
    let Some(next) = end.checked_add(1) else {
        return ContentRangeState::Unknown;
    };
    if start != 0 || end < start {
        return ContentRangeState::Unknown;
    }
    let expected_len = next;
    if total == "*" {
        return ContentRangeState::UnknownTotal { expected_len };
    }
    let Ok(total) = total.parse::<u64>() else {
        return ContentRangeState::Unknown;
    };
    if next > total {
        return ContentRangeState::Unknown;
    }
    if next < total {
        ContentRangeState::HasUnreturnedTail { expected_len }
    } else {
        ContentRangeState::Complete { expected_len }
    }
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
    if !policy.video_enabled && looks_like_direct_video_url(url) {
        return LinkPreviewResolverStatus::Unsupported;
    }
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
        if policy
            .blocked_hosts
            .iter()
            .any(|pattern| pattern.matches(host))
        {
            return LinkPreviewResolverStatus::Blocked;
        }
        if !policy.allowed_hosts.is_empty()
            && !policy
                .allowed_hosts
                .iter()
                .any(|pattern| pattern.matches(host))
        {
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
            video: None,
        },
        image,
    ))
}

fn safe_preview_image_media_type(value: &str) -> Option<PreviewImageMediaType> {
    let media_type = value
        .split(';')
        .next()
        .unwrap_or(value)
        .trim()
        .parse()
        .ok()?;
    Some(media_type)
}

fn sniff_safe_preview_image_media_type(bytes: &[u8]) -> Option<PreviewImageMediaType> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(PreviewImageMediaType::Png);
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some(PreviewImageMediaType::Jpeg);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(PreviewImageMediaType::Gif);
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(PreviewImageMediaType::Webp);
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

#[derive(Default)]
struct HtmlHeadEndScanner {
    cursor: usize,
    seen_head_start: bool,
    state: HtmlHeadScanState,
}

impl HtmlHeadEndScanner {
    fn scan(&mut self, html: &[u8]) -> Option<usize> {
        while self.cursor < html.len() {
            let state = std::mem::take(&mut self.state);
            match state {
                HtmlHeadScanState::Data => {
                    if html[self.cursor] != b'<' {
                        self.cursor += 1;
                        self.state = HtmlHeadScanState::Data;
                        continue;
                    }
                    if comment_start_is_pending(html, self.cursor) {
                        self.state = HtmlHeadScanState::Data;
                        break;
                    }
                    if html[self.cursor..].starts_with(b"<!--") {
                        self.cursor += b"<!--".len();
                        self.state = HtmlHeadScanState::Comment { dash_count: 0 };
                        continue;
                    }
                    self.cursor += 1;
                    self.state = HtmlHeadScanState::Tag(HtmlTagScanner::default());
                }
                HtmlHeadScanState::Tag(mut tag) => {
                    let byte = html[self.cursor];
                    self.cursor += 1;
                    if let Some(quote) = tag.quote {
                        if byte == quote {
                            tag.quote = None;
                        }
                        self.state = HtmlHeadScanState::Tag(tag);
                        continue;
                    }
                    match byte {
                        b'\'' | b'"' => {
                            tag.quote = Some(byte);
                            self.state = HtmlHeadScanState::Tag(tag);
                        }
                        b'>' => {
                            if tag.is_valid_head_end_tag() && self.seen_head_start {
                                self.state = HtmlHeadScanState::Data;
                                return Some(self.cursor);
                            }
                            if tag.is_head_start_tag() {
                                self.seen_head_start = true;
                            }
                            self.state = tag
                                .raw_text_closing_name()
                                .map(|closing_name| HtmlHeadScanState::RawText { closing_name })
                                .unwrap_or(HtmlHeadScanState::Data);
                        }
                        _ => {
                            tag.consume_unquoted(byte);
                            self.state = HtmlHeadScanState::Tag(tag);
                        }
                    }
                }
                HtmlHeadScanState::Comment { mut dash_count } => {
                    let byte = html[self.cursor];
                    self.cursor += 1;
                    match byte {
                        b'-' => dash_count += 1,
                        b'>' if dash_count >= 2 => {
                            self.state = HtmlHeadScanState::Data;
                            continue;
                        }
                        _ => dash_count = 0,
                    }
                    self.state = HtmlHeadScanState::Comment { dash_count };
                }
                HtmlHeadScanState::RawText { closing_name } => {
                    if html[self.cursor] != b'<' {
                        self.cursor += 1;
                        self.state = HtmlHeadScanState::RawText { closing_name };
                        continue;
                    }
                    match end_tag_end_index_at(html, self.cursor, closing_name) {
                        EndTagMatch::Complete(end) => {
                            self.cursor = end;
                            self.state = HtmlHeadScanState::Data;
                        }
                        EndTagMatch::Incomplete => {
                            self.state = HtmlHeadScanState::RawText { closing_name };
                            break;
                        }
                        EndTagMatch::NotMatch => {
                            self.cursor += 1;
                            self.state = HtmlHeadScanState::RawText { closing_name };
                        }
                    }
                }
            }
        }
        None
    }
}

#[derive(Default)]
enum HtmlHeadScanState {
    #[default]
    Data,
    Tag(HtmlTagScanner),
    Comment {
        dash_count: usize,
    },
    RawText {
        closing_name: &'static [u8],
    },
}

#[derive(Default)]
struct HtmlTagScanner {
    name: Vec<u8>,
    name_done: bool,
    invalid_end_tag_tail: bool,
    quote: Option<u8>,
}

impl HtmlTagScanner {
    fn consume_unquoted(&mut self, byte: u8) {
        if self.name_done {
            if !byte.is_ascii_whitespace() {
                self.invalid_end_tag_tail = true;
            }
            return;
        }
        if self.name.is_empty() && byte == b'/' {
            self.name.push(byte);
            return;
        }
        if !byte.is_ascii_whitespace() && byte != b'/' {
            self.name.push(byte.to_ascii_lowercase());
            return;
        }
        self.name_done = true;
        if !byte.is_ascii_whitespace() {
            self.invalid_end_tag_tail = true;
        }
    }

    fn is_valid_head_end_tag(&self) -> bool {
        self.name == b"/head" && !self.invalid_end_tag_tail
    }

    fn is_head_start_tag(&self) -> bool {
        self.name == b"head"
    }

    fn raw_text_closing_name(&self) -> Option<&'static [u8]> {
        match self.name.as_slice() {
            b"script" => Some(b"script"),
            b"style" => Some(b"style"),
            b"title" => Some(b"title"),
            b"textarea" => Some(b"textarea"),
            _ => None,
        }
    }
}

fn comment_start_is_pending(html: &[u8], start: usize) -> bool {
    const COMMENT_START: &[u8] = b"<!--";
    let remaining = &html[start..];
    remaining.len() < COMMENT_START.len() && COMMENT_START.starts_with(remaining)
}

enum EndTagMatch {
    Complete(usize),
    Incomplete,
    NotMatch,
}

fn end_tag_end_index_at(html: &[u8], start: usize, name: &[u8]) -> EndTagMatch {
    if html.get(start) != Some(&b'<') {
        return EndTagMatch::NotMatch;
    }
    let mut index = start + 1;
    if index >= html.len() {
        return EndTagMatch::Incomplete;
    }
    if html[index] != b'/' {
        return EndTagMatch::NotMatch;
    }
    index += 1;
    for expected in name {
        if index >= html.len() {
            return EndTagMatch::Incomplete;
        }
        if !html[index].eq_ignore_ascii_case(expected) {
            return EndTagMatch::NotMatch;
        }
        index += 1;
    }
    while index < html.len() && html[index].is_ascii_whitespace() {
        index += 1;
    }
    if index >= html.len() {
        return EndTagMatch::Incomplete;
    }
    if html[index] == b'>' {
        EndTagMatch::Complete(index + 1)
    } else {
        EndTagMatch::NotMatch
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use crate::server::routes::websocket::link_preview_telemetry::recorded_events;
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

    async fn test_media_cache(
        storage: Arc<dyn crate::storage::BlobStorage>,
    ) -> LinkPreviewMediaCache {
        let (cache, _) = test_media_cache_with_base_url(storage, "https://waddle.example").await;
        cache
    }

    async fn test_media_cache_with_base_url(
        storage: Arc<dyn crate::storage::BlobStorage>,
        base_url: &str,
    ) -> (LinkPreviewMediaCache, DatabasePool) {
        let db_pool = DatabasePool::new(DatabaseConfig::default(), PoolConfig)
            .await
            .expect("db pool");
        MigrationRunner::global()
            .run(db_pool.global())
            .await
            .expect("migrations");
        let cache = LinkPreviewMediaCache::new(
            storage,
            base_url,
            db_pool.global_actor().clone(),
            "alice@example.com".parse().expect("jid"),
        );
        (cache, db_pool)
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

    #[test]
    fn host_block_patterns_override_allowed_https_targets() {
        let policy = LinkPreviewResolverPolicy {
            blocked_hosts: vec!["*.example.com".parse().expect("pattern")],
            ..Default::default()
        };
        let blocked = Url::parse("https://news.example.com/article").expect("url");
        let allowed = Url::parse("https://example.net/article").expect("url");

        assert_eq!(
            classify_url_with_policy(&blocked, &policy),
            LinkPreviewResolverStatus::Blocked
        );
        assert_eq!(
            classify_url_with_policy(&allowed, &policy),
            LinkPreviewResolverStatus::Ready
        );
    }

    #[test]
    fn non_empty_host_allow_patterns_block_unlisted_hosts() {
        let policy = LinkPreviewResolverPolicy {
            allowed_hosts: vec!["example.com".parse().expect("pattern")],
            ..Default::default()
        };
        let allowed = Url::parse("https://example.com/article").expect("url");
        let blocked = Url::parse("https://elsewhere.example/article").expect("url");

        assert_eq!(
            classify_url_with_policy(&allowed, &policy),
            LinkPreviewResolverStatus::Ready
        );
        assert_eq!(
            classify_url_with_policy(&blocked, &policy),
            LinkPreviewResolverStatus::Blocked
        );
    }

    #[tokio::test]
    async fn disabled_video_policy_rejects_direct_video_urls_before_fetch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/clip.mp4"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("not fetched", "video/mp4"))
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            video_enabled: false,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/clip.mp4", server.uri())).expect("url");

        assert_eq!(
            resolve_link_preview(&url, &policy).await,
            LinkPreviewResolverOutcome::Unsupported
        );
    }

    #[tokio::test]
    async fn detects_direct_video_file_and_returns_typed_video_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/clip.mp4"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(vec![0u8; 4096], "video/mp4"))
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/clip.mp4", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        let video = metadata.video.expect("direct video metadata");
        assert_eq!(video.media_type, DirectVideoMediaType::Mp4);
        assert_eq!(video.url.as_str(), url.as_str());
        assert_eq!(video.size, Some(4096));
        assert!(
            metadata.image.is_none(),
            "direct video has no preview image"
        );
        assert!(metadata.title.is_none());
    }

    #[tokio::test]
    async fn direct_video_total_size_uses_content_range_total_on_partial_response() {
        // A Range request can return 206 Partial Content whose Content-Length is
        // only the slice; the authoritative total is the Content-Range suffix.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/clip.mp4"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-range", "bytes 0-255/1048576")
                    .set_body_raw(vec![0u8; 256], "video/mp4"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/clip.mp4", server.uri())).expect("url");

        let LinkPreviewResolverOutcome::Ready(metadata) = resolve_link_preview(&url, &policy).await
        else {
            panic!("expected ready outcome");
        };
        let video = metadata.video.expect("direct video metadata");
        assert_eq!(video.size, Some(1_048_576));
    }

    #[tokio::test]
    async fn rejects_video_content_type_at_non_video_path_without_fetching_body() {
        // A provider endpoint that returns a video content-type for a non-file
        // URL (e.g. an embed/watch page) must not be treated as a direct video.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/watch"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("not a file", "video/mp4"))
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/watch", server.uri())).expect("url");

        assert_eq!(
            resolve_link_preview(&url, &policy).await,
            LinkPreviewResolverOutcome::Unsupported
        );
    }

    #[tokio::test]
    async fn html_embed_page_at_video_extension_is_never_treated_as_direct_video() {
        // A provider embed/iframe page (text/html) served at a video-looking URL
        // must follow the HTML metadata path, never the direct-video path —
        // content-type is authoritative, so no file-sharing stamping can result.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/clip.mp4"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"<html><head>
                      <meta property="og:title" content="Embedded player">
                    </head><body><iframe src="https://provider.example/embed"></iframe></body></html>"#,
                "text/html; charset=utf-8",
            ))
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/clip.mp4", server.uri())).expect("url");

        let LinkPreviewResolverOutcome::Ready(metadata) = resolve_link_preview(&url, &policy).await
        else {
            panic!("expected html metadata outcome");
        };
        assert!(
            metadata.video.is_none(),
            "html embed pages must never produce direct-video metadata"
        );
        assert_eq!(metadata.title.as_deref(), Some("Embedded player"));
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

    fn scan_head_end_one_byte_at_a_time(html: &str) -> Option<usize> {
        let mut scanner = HtmlHeadEndScanner::default();
        let mut buffer = Vec::with_capacity(html.len());
        for byte in html.as_bytes() {
            buffer.push(*byte);
            if let Some(end) = scanner.scan(&buffer) {
                return Some(end);
            }
        }
        scanner.scan(&buffer)
    }

    async fn assert_single_range_request(server: &MockServer, expected: &str) {
        let requests = server
            .received_requests()
            .await
            .expect("received requests should be available");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0]
                .headers
                .get("range")
                .and_then(|value| value.to_str().ok()),
            Some(expected)
        );
    }

    async fn assert_invalid_partial_html_response_fails(content_range: Option<&str>) {
        let server = MockServer::start().await;
        let body = r#"<html><head>
                  <meta property="og:title" content="Invalid partial metadata">
                </head><body></body></html>"#;
        let mut response = ResponseTemplate::new(206)
            .insert_header("content-type", "text/html; charset=utf-8")
            .insert_header("content-length", body.len().to_string())
            .set_body_raw(body, "text/html; charset=utf-8");
        if let Some(content_range) = content_range {
            response = response.insert_header("content-range", content_range);
        }
        Mock::given(method("GET"))
            .and(path("/invalid-partial"))
            .respond_with(response)
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 256,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/invalid-partial", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Failed);
        assert_single_range_request(&server, "bytes=0-255").await;
    }

    #[test]
    fn head_end_scanner_ignores_split_false_markers_in_comment_and_script() {
        let html = r#"<html><head><!-- </head> --><script>const marker = "</head>";</script><meta property="og:title" content="Recovered"></head><body></body></html>"#;
        let expected = html.rfind("</head>").expect("head end") + "</head>".len();

        assert_eq!(scan_head_end_one_byte_at_a_time(html), Some(expected));
    }

    #[test]
    fn head_end_scanner_accepts_split_whitespace_before_close_bracket() {
        let html = "<html><head><title>not </head></title></head \t><body></body></html>";
        let expected = html.find("</head \t>").expect("head end") + "</head \t>".len();

        assert_eq!(scan_head_end_one_byte_at_a_time(html), Some(expected));
    }

    #[test]
    fn head_end_scanner_does_not_treat_custom_tags_as_head_or_raw_text() {
        let html = "<html><head-section></head><head profile=\"https://example.test\"><script-widget></script-widget><meta property=\"og:title\" content=\"Recovered\"></head><body></body></html>";
        let expected = html.rfind("</head>").expect("head end") + "</head>".len();

        assert_eq!(scan_head_end_one_byte_at_a_time(html), Some(expected));
    }

    #[test]
    fn head_end_scanner_treats_slash_after_raw_text_tag_name_as_raw_text() {
        let html = r#"<html><head><script/type>const marker = "</head>";</script><meta property="og:title" content="Recovered"></head><body></body></html>"#;
        let expected = html.rfind("</head>").expect("head end") + "</head>".len();

        assert_eq!(scan_head_end_one_byte_at_a_time(html), Some(expected));
    }

    #[tokio::test]
    async fn extracts_head_metadata_when_full_html_exceeds_byte_cap() {
        let server = MockServer::start().await;
        let full_body = format!(
            r#"<html><head>
                  <meta property="og:title" content="GitHub PR">
                  <meta property="og:description" content="Large page with early metadata">
                </head><body>{}</body></html>"#,
            "x".repeat(1024)
        );
        let ranged_body = &full_body[..256];
        Mock::given(method("GET"))
            .and(path("/pull/838"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-type", "text/html; charset=utf-8")
                    .insert_header("content-length", ranged_body.len().to_string())
                    .insert_header("content-range", format!("bytes 0-255/{}", full_body.len()))
                    .set_body_raw(ranged_body, "text/html; charset=utf-8"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 256,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/pull/838", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        assert_eq!(metadata.title.as_deref(), Some("GitHub PR"));
        assert_eq!(
            metadata.description.as_deref(),
            Some("Large page with early metadata")
        );
        assert_single_range_request(&server, "bytes=0-255").await;
    }

    #[tokio::test]
    async fn ignores_false_head_end_markers_before_later_metadata() {
        let server = MockServer::start().await;
        let body = r#"<html><head>
                  <meta name="test" content="</head>">
                  <!-- </head> -->
                  <script>const marker = "</head>";</script>
                  <meta property="og:title" content="Recovered metadata">
                  <meta property="og:description" content="After fake head markers">
                </head><body></body></html>"#;
        Mock::given(method("GET"))
            .and(path("/article"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/html; charset=utf-8"))
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
        assert_eq!(metadata.title.as_deref(), Some("Recovered metadata"));
        assert_eq!(
            metadata.description.as_deref(),
            Some("After fake head markers")
        );
    }

    #[tokio::test]
    async fn accepts_head_end_tag_with_whitespace_before_close_bracket() {
        let server = MockServer::start().await;
        let full_body = format!(
            r#"<html><head>
                  <meta property="og:title" content="Whitespace head close">
                </head 	><body>{}</body></html>"#,
            "x".repeat(1024)
        );
        let ranged_body = &full_body[..256];
        Mock::given(method("GET"))
            .and(path("/article"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-type", "text/html; charset=utf-8")
                    .insert_header("content-length", ranged_body.len().to_string())
                    .insert_header("content-range", format!("bytes 0-255/{}", full_body.len()))
                    .set_body_raw(ranged_body, "text/html; charset=utf-8"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 256,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/article", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        assert_eq!(metadata.title.as_deref(), Some("Whitespace head close"));
        assert_single_range_request(&server, "bytes=0-255").await;
    }

    #[tokio::test]
    async fn exact_size_partial_html_without_completed_head_returns_failed() {
        let server = MockServer::start().await;
        let mut ranged_body = r#"<html><head>
                  <meta property="og:title" content="Incomplete head metadata">
                "#
        .to_string();
        ranged_body.push_str(&"x".repeat(256 - ranged_body.len()));
        Mock::given(method("GET"))
            .and(path("/partial"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-type", "text/html; charset=utf-8")
                    .insert_header("content-length", ranged_body.len().to_string())
                    .insert_header("content-range", "bytes 0-255/1024")
                    .set_body_raw(ranged_body, "text/html; charset=utf-8"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 256,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/partial", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Failed);
        assert_single_range_request(&server, "bytes=0-255").await;
    }

    #[tokio::test]
    async fn partial_html_without_content_range_and_without_completed_head_returns_failed() {
        let server = MockServer::start().await;
        let mut ranged_body = r#"<html><head>
                  <meta property="og:title" content="Missing content range metadata">
                "#
        .to_string();
        ranged_body.push_str(&"x".repeat(256 - ranged_body.len()));
        Mock::given(method("GET"))
            .and(path("/partial-missing-content-range"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-type", "text/html; charset=utf-8")
                    .insert_header("content-length", ranged_body.len().to_string())
                    .set_body_raw(ranged_body, "text/html; charset=utf-8"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 256,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url =
            Url::parse(&format!("{}/partial-missing-content-range", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Failed);
        assert_single_range_request(&server, "bytes=0-255").await;
    }

    #[tokio::test]
    async fn partial_html_with_malformed_content_range_and_without_completed_head_returns_failed() {
        let server = MockServer::start().await;
        let mut ranged_body = r#"<html><head>
                  <meta property="og:title" content="Malformed content range metadata">
                "#
        .to_string();
        ranged_body.push_str(&"x".repeat(256 - ranged_body.len()));
        Mock::given(method("GET"))
            .and(path("/partial-malformed-content-range"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-type", "text/html; charset=utf-8")
                    .insert_header("content-length", ranged_body.len().to_string())
                    .insert_header("content-range", "bytes invalid")
                    .set_body_raw(ranged_body, "text/html; charset=utf-8"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 256,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url =
            Url::parse(&format!("{}/partial-malformed-content-range", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Failed);
        assert_single_range_request(&server, "bytes=0-255").await;
    }

    #[tokio::test]
    async fn partial_html_with_non_leading_content_range_and_without_completed_head_returns_failed()
    {
        let server = MockServer::start().await;
        let mut ranged_body = r#"<html><head>
                  <meta property="og:title" content="Non-leading content range metadata">
                "#
        .to_string();
        ranged_body.push_str(&"x".repeat(256 - ranged_body.len()));
        Mock::given(method("GET"))
            .and(path("/partial-non-leading-content-range"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-type", "text/html; charset=utf-8")
                    .insert_header("content-length", ranged_body.len().to_string())
                    .insert_header("content-range", "bytes 100-355/356")
                    .set_body_raw(ranged_body, "text/html; charset=utf-8"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 256,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!(
            "{}/partial-non-leading-content-range",
            server.uri()
        ))
        .expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Failed);
        assert_single_range_request(&server, "bytes=0-255").await;
    }

    #[tokio::test]
    async fn partial_html_with_impossible_content_range_and_without_completed_head_returns_failed()
    {
        let server = MockServer::start().await;
        let mut ranged_body = r#"<html><head>
                  <meta property="og:title" content="Impossible content range metadata">
                "#
        .to_string();
        ranged_body.push_str(&"x".repeat(256 - ranged_body.len()));
        Mock::given(method("GET"))
            .and(path("/partial-impossible-content-range"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-type", "text/html; charset=utf-8")
                    .insert_header("content-length", ranged_body.len().to_string())
                    .insert_header("content-range", "bytes 0-255/42")
                    .set_body_raw(ranged_body, "text/html; charset=utf-8"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 256,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!(
            "{}/partial-impossible-content-range",
            server.uri()
        ))
        .expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Failed);
        assert_single_range_request(&server, "bytes=0-255").await;
    }

    #[tokio::test]
    async fn partial_html_without_content_range_and_completed_head_returns_failed() {
        assert_invalid_partial_html_response_fails(None).await;
    }

    #[tokio::test]
    async fn partial_html_with_malformed_content_range_and_completed_head_returns_failed() {
        assert_invalid_partial_html_response_fails(Some("bytes invalid")).await;
    }

    #[tokio::test]
    async fn partial_html_with_non_leading_content_range_and_completed_head_returns_failed() {
        assert_invalid_partial_html_response_fails(Some("bytes 100-355/356")).await;
    }

    #[tokio::test]
    async fn partial_html_with_impossible_content_range_and_completed_head_returns_failed() {
        assert_invalid_partial_html_response_fails(Some("bytes 0-255/42")).await;
    }

    #[tokio::test]
    async fn complete_exact_size_partial_html_without_completed_head_uses_metadata() {
        let server = MockServer::start().await;
        let mut ranged_body = r#"<html><head>
                  <meta property="og:title" content="Complete partial metadata">
                "#
        .to_string();
        ranged_body.push_str(&"x".repeat(256 - ranged_body.len()));
        Mock::given(method("GET"))
            .and(path("/complete-partial"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-type", "text/html; charset=utf-8")
                    .insert_header("content-length", ranged_body.len().to_string())
                    .insert_header("content-range", "bytes 0-255/256")
                    .set_body_raw(ranged_body, "text/html; charset=utf-8"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 256,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/complete-partial", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        assert_eq!(metadata.title.as_deref(), Some("Complete partial metadata"));
        assert_single_range_request(&server, "bytes=0-255").await;
    }

    #[tokio::test]
    async fn complete_exact_size_partial_html_with_short_body_returns_failed() {
        let server = MockServer::start().await;
        let body = r#"<html><head>
                  <meta property="og:title" content="Short forged complete metadata">
                "#;
        Mock::given(method("GET"))
            .and(path("/short-complete-partial"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-type", "text/html; charset=utf-8")
                    .insert_header("content-length", body.len().to_string())
                    .insert_header("content-range", "bytes 0-255/256")
                    .set_body_raw(body, "text/html; charset=utf-8"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 256,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/short-complete-partial", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Failed);
        assert_single_range_request(&server, "bytes=0-255").await;
    }

    #[tokio::test]
    async fn complete_exact_size_partial_html_with_long_body_returns_failed() {
        let server = MockServer::start().await;
        let mut body = r#"<html><head>
                  <meta property="og:title" content="Long forged complete metadata">
                </head>"#
            .to_string();
        body.push_str(&"x".repeat(300 - body.len()));
        Mock::given(method("GET"))
            .and(path("/long-complete-partial"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-type", "text/html; charset=utf-8")
                    .insert_header("content-length", body.len().to_string())
                    .insert_header("content-range", "bytes 0-255/256")
                    .set_body_raw(body, "text/html; charset=utf-8"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 512,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/long-complete-partial", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Failed);
        assert_single_range_request(&server, "bytes=0-511").await;
    }

    #[tokio::test]
    async fn unknown_total_partial_html_with_completed_head_uses_metadata() {
        let server = MockServer::start().await;
        let mut ranged_body = r#"<html><head>
                  <meta property="og:title" content="Unknown total metadata">
                </head>"#
            .to_string();
        ranged_body.push_str(&"x".repeat(256 - ranged_body.len()));
        Mock::given(method("GET"))
            .and(path("/unknown-total-complete-head"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-type", "text/html; charset=utf-8")
                    .insert_header("content-length", ranged_body.len().to_string())
                    .insert_header("content-range", "bytes 0-255/*")
                    .set_body_raw(ranged_body, "text/html; charset=utf-8"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 256,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url =
            Url::parse(&format!("{}/unknown-total-complete-head", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        assert_eq!(metadata.title.as_deref(), Some("Unknown total metadata"));
        assert_single_range_request(&server, "bytes=0-255").await;
    }

    #[tokio::test]
    async fn unknown_total_partial_html_without_completed_head_returns_failed() {
        let server = MockServer::start().await;
        let mut ranged_body = r#"<html><head>
                  <meta property="og:title" content="Unknown total incomplete metadata">
                "#
        .to_string();
        ranged_body.push_str(&"x".repeat(256 - ranged_body.len()));
        Mock::given(method("GET"))
            .and(path("/unknown-total-incomplete-head"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-type", "text/html; charset=utf-8")
                    .insert_header("content-length", ranged_body.len().to_string())
                    .insert_header("content-range", "bytes 0-255/*")
                    .set_body_raw(ranged_body, "text/html; charset=utf-8"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 256,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url =
            Url::parse(&format!("{}/unknown-total-incomplete-head", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Failed);
        assert_single_range_request(&server, "bytes=0-255").await;
    }

    #[tokio::test]
    async fn xhtml_false_head_marker_in_cdata_does_not_truncate_later_metadata() {
        let server = MockServer::start().await;
        let full_body = format!(
            r#"<html><head>
                  <script><![CDATA["</script></head>"]]></script>
                  <meta property="og:title" content="XHTML metadata">
                </head><body>{}</body></html>"#,
            "x".repeat(1024)
        );
        let ranged_body = &full_body[..256];
        Mock::given(method("GET"))
            .and(path("/article.xhtml"))
            .respond_with(
                ResponseTemplate::new(206)
                    .insert_header("content-type", "application/xhtml+xml; charset=utf-8")
                    .insert_header("content-length", ranged_body.len().to_string())
                    .insert_header("content-range", format!("bytes 0-255/{}", full_body.len()))
                    .set_body_raw(ranged_body, "application/xhtml+xml; charset=utf-8"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            max_bytes: 256,
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/article.xhtml", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        assert_eq!(metadata.title.as_deref(), Some("XHTML metadata"));
        assert_single_range_request(&server, "bytes=0-255").await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetches_safe_preview_image_into_content_addressed_waddle_storage() {
        let _events_guard = recorded_events::async_lock().await;
        recorded_events::clear();
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
        let (media_cache, db_pool) =
            test_media_cache_with_base_url(storage.clone(), "https://waddle.example").await;
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            media_cache: Some(media_cache.clone()),
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
        assert!(image.url.path().starts_with("/api/files/"));
        assert!(image
            .url
            .path()
            .ends_with(&format!("/link-preview-{expected_hash}.png")));
        assert_eq!(image.media_type, PreviewImageMediaType::Png);
        assert_eq!(image.width, Some(640));
        assert_eq!(image.height, Some(360));
        assert_eq!(image.alt.as_deref(), Some("Article screenshot"));
        let slot_id = image
            .url
            .path_segments()
            .and_then(|mut segments| segments.nth(2))
            .expect("slot id");
        let db = db_pool.global().guard().await.expect("db");
        let mut rows = db
            .query(
                "SELECT filename, status, storage_key FROM upload_slots WHERE id = ?",
                crate::db_params![slot_id],
            )
            .await
            .expect("slot row");
        let row = rows
            .next()
            .await
            .expect("next row")
            .expect("slot row exists");
        assert_eq!(
            row.get::<String>(0).expect("filename"),
            format!("link-preview-{expected_hash}.png")
        );
        assert_eq!(row.get::<String>(1).expect("status"), "uploaded");
        assert_eq!(
            row.get::<Option<String>>(2).expect("storage_key"),
            Some(expected_key.clone())
        );
        assert!(rows.next().await.expect("next row").is_none());
        let (stored, meta) = storage.get(&expected_key).await.expect("cached bytes");
        assert_eq!(stored, image_bytes);
        assert_eq!(meta.content_type, "image/png");
        assert!(
            recorded_events::take().contains(&LinkPreviewTelemetryEvent::CacheMiss),
            "first content-addressed image store must emit cache_miss telemetry"
        );

        recorded_events::clear();
        let second_outcome = resolve_link_preview(&url, &policy).await;
        let LinkPreviewResolverOutcome::Ready(second_metadata) = second_outcome else {
            panic!("expected ready outcome, got {second_outcome:?}");
        };
        assert!(second_metadata.image.is_some());
        assert!(
            recorded_events::take().contains(&LinkPreviewTelemetryEvent::CacheHit),
            "reusing an existing content-addressed image must emit cache_hit telemetry"
        );
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
            media_cache: Some(test_media_cache(storage.clone()).await),
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
    async fn oversized_preview_image_content_length_degrades_to_text_metadata() {
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
                    .set_body_bytes({
                        let mut bytes = vec![b'x'; 1024];
                        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
                        bytes
                    }),
            )
            .mount(&server)
            .await;
        let storage_dir =
            std::env::temp_dir().join(format!("waddle-link-preview-{}", uuid::Uuid::new_v4()));
        let storage: Arc<dyn crate::storage::BlobStorage> =
            Arc::new(crate::storage::LocalStorage::new(storage_dir));
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            media_cache: Some(test_media_cache(storage.clone()).await),
            max_image_bytes: 16,
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
    async fn oversized_preview_image_stream_degrades_to_text_metadata() {
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
                    .set_body_bytes(bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\nbody-over-cap")),
            )
            .mount(&server)
            .await;
        let storage_dir =
            std::env::temp_dir().join(format!("waddle-link-preview-{}", uuid::Uuid::new_v4()));
        let storage: Arc<dyn crate::storage::BlobStorage> =
            Arc::new(crate::storage::LocalStorage::new(storage_dir));
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            media_cache: Some(test_media_cache(storage.clone()).await),
            max_image_bytes: 8,
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
            media_cache: Some(test_media_cache(storage.clone()).await),
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/article", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
            panic!("expected ready outcome, got {outcome:?}");
        };
        let expected_hash = hex::encode(Sha256::digest(image_bytes.as_ref()));
        let image = metadata.image.expect("cached image");
        assert!(image.url.path().starts_with("/api/files/"));
        assert!(image
            .url
            .path()
            .ends_with(&format!("/link-preview-{expected_hash}.png")));
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
            media_cache: Some(test_media_cache(storage).await),
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
            media_cache: Some(test_media_cache(storage).await),
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
            media_cache: Some(test_media_cache_with_base_url(storage, "not a url").await.0),
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
            media_cache: Some(test_media_cache(Arc::new(FailingPutStorage)).await),
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
            media_cache: Some(test_media_cache(storage).await),
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
            media_cache: Some(test_media_cache(storage).await),
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
            media_cache: Some(test_media_cache(storage).await),
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
    async fn oversized_content_length_returns_failed_when_range_is_ignored() {
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
    async fn redirect_into_admin_blocked_host_returns_blocked_without_following() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/redirect"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", "https://blocked.example/final"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            blocked_hosts: vec!["blocked.example".parse().expect("pattern")],
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/redirect", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Blocked);
    }

    #[tokio::test]
    async fn redirect_outside_admin_allowlist_returns_blocked_without_following() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/redirect"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", "https://elsewhere.example/final"),
            )
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            allowed_hosts: vec!["allowed.example".parse().expect("pattern")],
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/redirect", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Blocked);
    }

    #[tokio::test]
    async fn disabled_video_policy_rejects_redirected_video_url_before_fetch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/article"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/clip.mp4"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/clip.mp4"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("not fetched", "video/mp4"))
            .mount(&server)
            .await;
        let policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            video_enabled: false,
            ..Default::default()
        };
        let url = Url::parse(&format!("{}/article", server.uri())).expect("url");

        let outcome = resolve_link_preview(&url, &policy).await;

        assert_eq!(outcome.status(), LinkPreviewResolverStatus::Unsupported);
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
