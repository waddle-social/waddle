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
//! - **MIME:** allowlist of `image/png`, `image/jpeg`, `image/gif`,
//!   `image/webp` at the fetch boundary. Non-PNG bodies are decoded
//!   and re-encoded as PNG before the helper returns — XEP-0084 §3.2
//!   normatively reserves the `urn:xmpp:avatar:data` node for
//!   `image/png`, §4.1.1 mandates the metadata ItemID be the SHA-1 of
//!   the PNG bytes, and §4.2 requires "one of the formats MUST be
//!   image/png to ensure interoperability." The header parses into
//!   `AllowedMime` (gate); the body is magic-byte sniffed against
//!   that declared format (defense against malformed/buggy servers
//!   that mis-label a body — not an adversarial defense, since the
//!   header itself is attacker-controlled); then the bytes are
//!   transcoded to PNG so callers always see a single canonical
//!   format on the wire.
//! - **Timeouts:** 5s connect, 10s total.
//! - **Retries:** one retry on transient failures (5xx, connect
//!   timeout, network error). No backoff.
//!
//! All failure modes are typed via `FetchError` so callers can map
//! into the persisted `users.last_avatar_fetch_error` enum without
//! parsing log strings.

use std::io::Cursor;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use image::{ImageFormat, ImageReader};
use reqwest::header::CONTENT_TYPE;
use reqwest::redirect;
use reqwest::Client;
use thiserror::Error;
use tracing::{debug, warn};
use url::{Host, Url};

const MAX_BYTES: usize = 100 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Image formats we accept on the avatar fetch path. Internal to
/// the fetcher: callers always see a transcoded PNG (XEP-0084 §3.2),
/// so this enum exists purely to gate inputs and dispatch the
/// magic-byte / decode logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AllowedMime {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl AllowedMime {
    /// Single source of truth for the allowlist. `from_header`,
    /// `as_mime`, and `matches_magic` all match exhaustively, so the
    /// compiler catches missing arms when a new variant is added.
    /// `ALL` is what the user-facing error message and tests iterate
    /// over — keep it in lockstep with the variants.
    const ALL: &'static [Self] = &[Self::Png, Self::Jpeg, Self::Gif, Self::Webp];

    /// Match a normalized (`split(';').next().trim().to_lowercase()`)
    /// `Content-Type` against the allowlist. `image/jpg` is rejected
    /// — RFC 6838 only registers `image/jpeg`. If a real-world IdP
    /// trips on this in practice, accept it explicitly with a test
    /// rather than loosening the match.
    fn from_header(s: &str) -> Option<Self> {
        match s {
            "image/png" => Some(Self::Png),
            "image/jpeg" => Some(Self::Jpeg),
            "image/gif" => Some(Self::Gif),
            "image/webp" => Some(Self::Webp),
            _ => None,
        }
    }

    fn as_mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }

    /// Comma-separated MIME list for human-facing diagnostics. The
    /// `MimeRejected` `Display` calls this so the allowlist string
    /// has a single source of truth.
    fn allowlist_help() -> String {
        Self::ALL
            .iter()
            .map(|m| m.as_mime())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Verify the body's leading bytes match the format declared in
    /// the response header. This is a malformed-server defense (a
    /// server returning JPEG bytes with `Content-Type: image/png`),
    /// not an adversarial one — the header itself is attacker-
    /// controlled, and a malicious server can simply declare whatever
    /// type matches the body it wants to smuggle. The transcode step
    /// downstream is what makes the wire payload predictable.
    ///
    /// Each signature is the format's documented fixed prefix:
    /// - PNG: 8-byte signature, RFC 2083 §3.1.
    /// - JPEG: SOI + first marker, `FF D8 FF`. Subsequent bytes vary
    ///   by encoder (E0/E1 for JFIF/EXIF, DB for raw quantization
    ///   tables) so only the 3-byte prefix is fixed.
    /// - GIF: ASCII `GIF87a` or `GIF89a` (CompuServe spec §17).
    /// - WebP: RIFF container — `RIFF` magic, 4-byte LE chunk size,
    ///   `WEBP` form (RFC 9649 §2.1).
    fn matches_magic(self, bytes: &[u8]) -> bool {
        match self {
            Self::Png => bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
            Self::Jpeg => bytes.starts_with(&[0xff, 0xd8, 0xff]),
            Self::Gif => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
            Self::Webp => bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        }
    }
}

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
    #[error(
        "response Content-Type {mime:?} is not in the avatar allowlist ({list})",
        mime = .0,
        list = AllowedMime::allowlist_help()
    )]
    MimeRejected(Option<String>),
    #[error("response body did not start with the magic-byte signature for the declared MIME")]
    MagicByteMismatch,
    #[error("response exceeds {0}-byte cap")]
    SizeExceeded(usize),
    #[error("could not transcode source bytes to image/png: {0}")]
    TranscodeFailed(String),
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
            FetchError::TranscodeFailed(_) => "transcode_failed",
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
    if url.scheme() != "https" && !policy.allow_http_for_tests {
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

    // Defense in depth — when the test policy opts in to plaintext
    // HTTP (`allow_http_for_tests=true`), the URL must additionally
    // resolve only to loopback. Production policy
    // (`FetchPolicy::default`) leaves `allow_http_for_tests` false
    // and this branch is unreachable, but proving that statically
    // here narrows the test-only surface and gives static analysis
    // a clear "https or loopback" barrier.
    if url.scheme() != "https" {
        let all_loopback = pinned_addrs.iter().all(|a| a.ip().is_loopback());
        if !all_loopback {
            return Err(FetchError::InvalidScheme(url.scheme().to_string()));
        }
    }

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
    let declared = match header_mime.as_deref().and_then(AllowedMime::from_header) {
        Some(m) => m,
        None => return Err(FetchError::MimeRejected(header_mime)),
    };

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

    if !declared.matches_magic(&buf) {
        return Err(FetchError::MagicByteMismatch);
    }

    // XEP-0084 §3.2: the `urn:xmpp:avatar:data` node carries
    // `image/png` only; §4.1.1 ties the metadata `<info id>` to the
    // SHA-1 of the PNG bytes; §4.2 requires "one of the formats MUST
    // be image/png to ensure interoperability." Non-PNG sources are
    // decoded and re-encoded as PNG here so the publish chain never
    // sees a non-conformant payload.
    let png_bytes = match declared {
        AllowedMime::Png => buf,
        AllowedMime::Jpeg | AllowedMime::Gif | AllowedMime::Webp => {
            // Decode + encode is CPU-bound; offload from the Tokio
            // executor so a slow image doesn't starve other async
            // tasks (OIDC publishes also run from a `tokio::spawn`
            // alongside the chat WebSocket loop).
            tokio::task::spawn_blocking(move || transcode_to_png(&buf, declared))
                .await
                .map_err(|e| FetchError::TranscodeFailed(format!("spawn_blocking join: {e}")))??
        }
    };

    // The post-transcode size cap protects D1 storage (issue #363:
    // `pubsub_items.payload_xml TEXT` budget). PNG re-encoding can
    // expand a small lossy JPEG into a larger lossless PNG; reject
    // those rather than overflowing the row. The pre-transcode
    // streaming cap (above, around `policy.max_bytes`) is a separate
    // OOM defense: it keeps the fetch buffer bounded regardless of
    // what the upstream serves.
    if png_bytes.len() > policy.max_bytes {
        return Err(FetchError::SizeExceeded(policy.max_bytes));
    }

    debug!(url = %url, bytes = png_bytes.len(), source_mime = %declared.as_mime(), "avatar fetched and normalised to image/png");
    Ok(AvatarBytes {
        bytes: png_bytes,
        mime: AllowedMime::Png.as_mime().to_string(),
    })
}

/// Cap on decoded image dimensions. A small adversarial JPEG can
/// declare e.g. 65535×65535 in its SOF0 marker; without an explicit
/// limit `image::ImageDecoder` would allocate
/// `width * height * channels` bytes (≈16 GB at 65k²×4) before we
/// could reject it. 4096×4096 covers any sane avatar — the
/// XEP-0084 §3.1 SHOULD is 96×96 — and caps decoded RGBA at 64 MB.
const MAX_IMAGE_DIMENSION: u32 = 4096;

/// Decode `bytes` (declared as `source`) and re-encode as PNG. The
/// `image` crate's `ImageReader::set_format` is used to pin the
/// decoder to the format already verified by the magic-byte check
/// in `try_fetch`; this avoids the slower "try every codec" path
/// `with_guessed_format` would take. `image::Limits` bounds the
/// decoder's allocations so a malformed source can't OOM the
/// process before the decode completes.
fn transcode_to_png(bytes: &[u8], source: AllowedMime) -> Result<Vec<u8>, FetchError> {
    let image_format = match source {
        AllowedMime::Png => ImageFormat::Png,
        AllowedMime::Jpeg => ImageFormat::Jpeg,
        AllowedMime::Gif => ImageFormat::Gif,
        AllowedMime::Webp => ImageFormat::WebP,
    };
    let mut reader = ImageReader::new(Cursor::new(bytes));
    reader.set_format(image_format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    reader.limits(limits);
    let img = reader
        .decode()
        .map_err(|e| FetchError::TranscodeFailed(format!("decode {}: {e}", source.as_mime())))?;
    let mut out = Vec::with_capacity(bytes.len());
    img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .map_err(|e| FetchError::TranscodeFailed(format!("encode png: {e}")))?;
    Ok(out)
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
        assert_eq!(
            FetchError::TranscodeFailed("bad bytes".into()).kind(),
            "transcode_failed"
        );
    }

    #[test]
    fn mime_rejected_display_lists_every_allowlist_entry() {
        // The user-facing error message must enumerate the
        // allowlist; that text and `AllowedMime::ALL` have to stay in
        // sync. `allowlist_help` is the single source of truth — pin
        // the contract so a future variant addition doesn't slip
        // through with stale Display text.
        let display = FetchError::MimeRejected(Some("image/bmp".into())).to_string();
        for variant in AllowedMime::ALL {
            assert!(
                display.contains(variant.as_mime()),
                "Display must list {}: {display}",
                variant.as_mime()
            );
        }
    }

    #[test]
    fn allowed_mime_round_trips_each_allowlist_entry() {
        for (header, expected) in [
            ("image/png", AllowedMime::Png),
            ("image/jpeg", AllowedMime::Jpeg),
            ("image/gif", AllowedMime::Gif),
            ("image/webp", AllowedMime::Webp),
        ] {
            let parsed = AllowedMime::from_header(header).unwrap_or_else(|| {
                panic!("{header} must be in the allowlist");
            });
            assert_eq!(parsed, expected);
            assert_eq!(parsed.as_mime(), header);
        }
    }

    #[test]
    fn allowed_mime_rejects_unsupported_types() {
        // RFC 6838 only registers `image/jpeg`; some IdPs serve
        // `image/jpg` colloquially. We do NOT accept the alias —
        // adding it would mean we publish `image/jpg` downstream
        // (non-conformant per RFC 6838) and accept a body whose
        // declared type isn't actually a registered MIME. If a real
        // IdP trips on this, add a dedicated test alongside the
        // explicit branch.
        for header in [
            "image/jpg",
            "image/bmp",
            "image/tiff",
            "image/svg+xml",
            "image/avif",
            "application/pdf",
            "text/plain",
            "",
        ] {
            assert!(
                AllowedMime::from_header(header).is_none(),
                "{header:?} must NOT be in the allowlist"
            );
        }
    }

    #[test]
    fn allowed_mime_matches_per_format_magic_bytes() {
        let png = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00];
        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 0x00];
        let gif87 = b"GIF87a\x00\x00";
        let gif89 = b"GIF89a\x00\x00";
        // RIFF<4 LE size bytes>WEBPVP8 ...
        let webp = b"RIFF\x24\x00\x00\x00WEBPVP8 \x00";

        assert!(AllowedMime::Png.matches_magic(&png));
        assert!(AllowedMime::Jpeg.matches_magic(&jpeg));
        assert!(AllowedMime::Gif.matches_magic(gif87));
        assert!(AllowedMime::Gif.matches_magic(gif89));
        assert!(AllowedMime::Webp.matches_magic(webp));

        // Mismatched header vs body is the smuggling case the magic
        // check exists to defeat.
        assert!(!AllowedMime::Png.matches_magic(&jpeg));
        assert!(!AllowedMime::Jpeg.matches_magic(&png));
        assert!(!AllowedMime::Gif.matches_magic(&jpeg));
        assert!(!AllowedMime::Webp.matches_magic(&jpeg));
    }

    #[test]
    fn allowed_mime_magic_check_handles_short_body() {
        // Empty / very short bodies must never panic — `try_fetch`
        // streams the response and could in principle deliver a
        // truncated buffer. `starts_with` is bounds-safe; the WebP
        // arm explicitly checks `len >= 12`. Pin the contract.
        for short in [&[][..], &[0xff][..], &[0xff, 0xd8][..]] {
            assert!(!AllowedMime::Png.matches_magic(short));
            assert!(!AllowedMime::Jpeg.matches_magic(short));
            assert!(!AllowedMime::Gif.matches_magic(short));
            assert!(!AllowedMime::Webp.matches_magic(short));
        }
    }

    /// Encode a 2×2 red square as the requested format. Two-by-two
    /// because the JPEG encoder needs at least an 8×8 MCU's worth of
    /// data internally; one-by-one decodes correctly but produces a
    /// JFIF-style fixture that's larger than necessary. 2×2 is the
    /// smallest useful test image where every codec is happy.
    fn encode_test_image(format: image::ImageFormat) -> Vec<u8> {
        let img = image::ImageBuffer::from_pixel(2u32, 2u32, image::Rgb([255u8, 0, 0]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), format)
            .expect("encoder must succeed for 2×2 RGB fixture");
        out
    }

    #[test]
    fn transcode_to_png_round_trips_each_non_png_format() {
        for (source, format) in [
            (AllowedMime::Jpeg, image::ImageFormat::Jpeg),
            (AllowedMime::Gif, image::ImageFormat::Gif),
            (AllowedMime::Webp, image::ImageFormat::WebP),
        ] {
            let source_bytes = encode_test_image(format);
            assert!(
                source.matches_magic(&source_bytes),
                "{source:?} fixture must clear its own magic-byte gate"
            );
            let png = transcode_to_png(&source_bytes, source)
                .unwrap_or_else(|e| panic!("{source:?} → PNG must succeed for a real image: {e}"));
            assert!(
                AllowedMime::Png.matches_magic(&png),
                "{source:?} → PNG output must clear the PNG magic-byte gate"
            );
        }
    }

    #[test]
    fn transcode_to_png_rejects_corrupted_body() {
        // Magic bytes match JPEG but the rest is junk — the decoder
        // surfaces this as `TranscodeFailed` so the fetch path can
        // map it into the persisted error enum without panicking.
        let corrupted = b"\xff\xd8\xff\xe0not-a-real-jpeg";
        let result = transcode_to_png(corrupted, AllowedMime::Jpeg);
        assert!(
            matches!(result, Err(FetchError::TranscodeFailed(_))),
            "{result:?}"
        );
    }

    #[test]
    fn transcode_to_png_rejects_oversize_dimensions() {
        // A PNG IHDR can declare any 32-bit width/height. The
        // attacker's body is < 100 KB on the wire (passes the
        // streaming cap) but advertises billions of pixels; without
        // `image::Limits` the decoder would attempt to allocate
        // `width * height * channels` bytes and OOM the process.
        // Construct a PNG with a 65535×65535 IHDR — far above the
        // 4096×4096 cap — and assert it surfaces as
        // `TranscodeFailed`, not an allocation panic.
        let mut png = Vec::new();
        // PNG signature.
        png.extend_from_slice(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
        // IHDR chunk: length=13, type="IHDR", data=W(4)+H(4)+depth(1)+color(1)+compr(1)+filter(1)+interl(1).
        let ihdr_data: [u8; 13] = [
            0x00, 0x00, 0xff, 0xff, // width = 65535
            0x00, 0x00, 0xff, 0xff, // height = 65535
            0x08, 0x06, 0x00, 0x00, 0x00, // 8-bit RGBA, default compression/filter/interlace
        ];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&ihdr_data);
        // Bogus CRC is fine — the decoder rejects via the limit
        // check before CRC validation.
        png.extend_from_slice(&[0, 0, 0, 0]);

        let result = transcode_to_png(&png, AllowedMime::Png);
        assert!(
            matches!(result, Err(FetchError::TranscodeFailed(_))),
            "expected TranscodeFailed (limit exceeded), got {result:?}"
        );
    }
}
