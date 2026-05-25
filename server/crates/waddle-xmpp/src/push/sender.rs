//! RFC 8030 Web Push HTTP transport.
//!
//! The sender is intentionally narrow: it accepts a fully prepared
//! [`WebPushRequest`] (encrypted body, signed VAPID JWT, public-key
//! base64url, topic/TTL/urgency) and returns a typed
//! [`WebPushOutcome`]. Encryption and JWT signing live in
//! `super::encrypt` and `super::vapid` respectively — keeping the
//! transport free of crypto state means the publish-job worker can sign
//! once per `(kid, aud, sub)` and reuse the JWT across many devices.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue, AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE};
use tracing::{debug, warn};
use url::Url;

use super::types::{
    EncryptedPayload, EndpointHash, PushTopic, TransientFailure, VapidJwt, WebPushOutcome,
};

/// RFC 8030 `Urgency` header values (§5.3). Carried as a typed enum so
/// callers can't accidentally emit unknown values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Urgency {
    VeryLow,
    Low,
    Normal,
    High,
}

impl Urgency {
    fn as_header(self) -> &'static str {
        match self {
            Urgency::VeryLow => "very-low",
            Urgency::Low => "low",
            Urgency::Normal => "normal",
            Urgency::High => "high",
        }
    }
}

/// All inputs needed to make one RFC 8030 POST. Borrowed so callers can
/// reuse a [`VapidJwt`] across the fan-out without cloning per device.
pub struct WebPushRequest<'a> {
    pub endpoint: &'a Url,
    pub payload: &'a EncryptedPayload,
    pub vapid_jwt: &'a VapidJwt,
    /// RFC 8292 `k=` value: uncompressed P-256 public key, base64url
    /// no-pad. Produced once at boot via
    /// `super::vapid::vapid_k_header`.
    pub vapid_public_key_b64u: &'a str,
    pub topic: Option<&'a PushTopic>,
    pub ttl: u32,
    pub urgency: Urgency,
}

/// Transport-layer Web Push sender.
///
/// Implementors translate [`WebPushRequest`] → typed
/// [`WebPushOutcome`]; they MUST NOT return `Result` because every
/// failure mode is a `WebPushOutcome` variant. This keeps the
/// publish-job worker's match arms exhaustive.
pub trait WebPushSender: Send + Sync + 'static {
    fn send(
        &self,
        request: WebPushRequest<'_>,
    ) -> Pin<Box<dyn Future<Output = WebPushOutcome> + Send + '_>>;
}

const TTL_HEADER: HeaderName = HeaderName::from_static("ttl");
const URGENCY_HEADER: HeaderName = HeaderName::from_static("urgency");
const TOPIC_HEADER: HeaderName = HeaderName::from_static("topic");
const AES128GCM: &str = "aes128gcm";
const OCTET_STREAM: &str = "application/octet-stream";

#[derive(Debug, Clone)]
pub struct HttpWebPushSender {
    client: reqwest::Client,
}

impl HttpWebPushSender {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build HTTP Web Push client");
        Self { client }
    }

    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl Default for HttpWebPushSender {
    fn default() -> Self {
        Self::new()
    }
}

impl WebPushSender for HttpWebPushSender {
    fn send(
        &self,
        request: WebPushRequest<'_>,
    ) -> Pin<Box<dyn Future<Output = WebPushOutcome> + Send + '_>> {
        // Build the request synchronously so the spawned future owns
        // nothing borrowed from the caller. Authorization is the only
        // header that can fail to construct (length / invisible-char
        // rejection in `HeaderValue::from_str`); a failure there is a
        // bug in the caller's `vapid_public_key_b64u`, surfaced as
        // `BadRequest` rather than a panic.
        let endpoint = request.endpoint.clone();
        // Defense-in-depth: registration-time validation already
        // rejects non-https endpoints, but if a future caller bypasses
        // that path the sender refuses to send a VAPID-signed
        // `Authorization` header over a plaintext scheme. Loopback
        // is allowed so mockito-based test fixtures (which serve
        // `http://127.0.0.1:<port>/`) keep working — registration-time
        // validation rejects literal IPs in production anyway, so a
        // non-loopback http URL cannot reach here in a real
        // deployment.
        if endpoint.scheme() != "https" && !endpoint_is_loopback(&endpoint) {
            warn!(
                endpoint_hash = %EndpointHash::of(endpoint.as_str()),
                origin = endpoint.origin().ascii_serialization(),
                scheme = endpoint.scheme(),
                "Web Push refused: endpoint is not https",
            );
            return Box::pin(async move { WebPushOutcome::BadRequest { status: 0 } });
        }
        let body = request.payload.as_slice().to_vec();

        let auth_value = format!(
            "vapid t={}, k={}",
            request.vapid_jwt.as_str(),
            request.vapid_public_key_b64u,
        );
        let auth_header = match HeaderValue::from_str(&auth_value) {
            Ok(v) => v,
            Err(_) => {
                // Log only the truncated endpoint hash — the full
                // endpoint URL is a per-device bearer identifier;
                // anyone with log access could otherwise replay-send
                // to it.
                warn!(
                    endpoint_hash = %EndpointHash::of(endpoint.as_str()),
                    origin = endpoint.origin().ascii_serialization(),
                    "VAPID Authorization header is not a valid ASCII HTTP header value",
                );
                return Box::pin(async move { WebPushOutcome::BadRequest { status: 0 } });
            }
        };

        let topic_value = request
            .topic
            .and_then(|t| HeaderValue::from_str(t.as_str()).ok());
        let ttl_value = HeaderValue::from(request.ttl);
        let urgency_value = HeaderValue::from_static(request.urgency.as_header());

        let mut builder = self
            .client
            .post(endpoint.clone())
            .header(AUTHORIZATION, auth_header)
            .header(CONTENT_TYPE, OCTET_STREAM)
            .header(CONTENT_ENCODING, AES128GCM)
            .header(TTL_HEADER, ttl_value)
            .header(URGENCY_HEADER, urgency_value)
            .body(body);
        if let Some(topic) = topic_value {
            builder = builder.header(TOPIC_HEADER, topic);
        }

        Box::pin(async move {
            match builder.send().await {
                Ok(resp) => classify_response(resp).await,
                Err(err) => classify_transport_error(&endpoint, err),
            }
        })
    }
}

async fn classify_response(resp: reqwest::Response) -> WebPushOutcome {
    let status = resp.status();
    let code = status.as_u16();
    if status.is_success() {
        // Drain the body before returning so reqwest's underlying
        // hyper connection can be returned to the pool — without
        // this the next per-host send opens a fresh connection.
        let _ = resp.bytes().await;
        debug!(status = %status, "Web Push delivered");
        return WebPushOutcome::Delivered { status: code };
    }
    // Snapshot headers used by classification BEFORE draining the
    // body (which consumes `resp`).
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after);
    let www_authenticate = resp
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_ascii_lowercase());
    // Drain the body to allow connection reuse. Capping wouldn't
    // help here — reqwest buffers the whole body anyway; the goal
    // is to consume it before we return.
    let _ = resp.bytes().await;
    match code {
        // 404 / 410 are the textbook "subscription gone" signals
        // (RFC 8030 §6). 403 is NOT folded in: while FCM does use 403
        // for "VAPID key not authorized for this endpoint", it also
        // uses it for cluster-wide VAPID rejection (wrong `aud`,
        // expired `exp` past the relay's tolerance, malformed JWT)
        // which is a deployment bug affecting ALL devices, not a
        // per-device subscription expiry. Routing every 403 to
        // SubscriptionGone would mass-disable devices on a
        // configuration error. Keep 403 as `BadRequest` (permanent,
        // non-disabling) so the failure surfaces in the operator
        // audit without bricking the user base.
        404 | 410 => WebPushOutcome::SubscriptionGone { status: code },
        // 401 means "VAPID JWT rejected" per RFC 8292 §3. The most
        // common cause is clock skew (relay rejects our `exp`); we
        // only label it `ClockSkew` when the relay echoes a vapid
        // scheme in `WWW-Authenticate` (RFC 7235 §4.1), the canonical
        // signal that the JWT itself was at fault. Otherwise treat as
        // a generic permanent auth failure to avoid invalidating the
        // JWT cache on unrelated 401 conditions.
        401 if matches!(&www_authenticate, Some(v) if v.contains("vapid")) => {
            WebPushOutcome::ClockSkew { status: code }
        }
        401 | 403 => WebPushOutcome::BadRequest { status: code },
        413 => WebPushOutcome::PayloadTooLarge { status: code },
        429 => WebPushOutcome::RateLimited {
            status: code,
            retry_after,
        },
        // 400 is "our payload shape is wrong" — encoder bug, not a
        // per-device problem. Surfaces in attempts for operator
        // action; the device row stays active so the next publish
        // succeeds once the bug is fixed.
        400 => WebPushOutcome::BadRequest { status: code },
        500..=599 => WebPushOutcome::Transient {
            kind: TransientFailure::ServerError { status: code },
        },
        _ => WebPushOutcome::BadRequest { status: code },
    }
}

fn classify_transport_error(endpoint: &Url, err: reqwest::Error) -> WebPushOutcome {
    let endpoint_hash = EndpointHash::of(endpoint.as_str());
    let origin = endpoint.origin().ascii_serialization();
    if err.is_timeout() {
        warn!(endpoint_hash = %endpoint_hash, origin = origin, error = %err, "Web Push timeout");
        WebPushOutcome::Transient {
            kind: TransientFailure::Timeout,
        }
    } else {
        warn!(endpoint_hash = %endpoint_hash, origin = origin, error = %err, "Web Push transport failure");
        WebPushOutcome::Transient {
            kind: TransientFailure::Network,
        }
    }
}

/// Is the endpoint host a loopback address? Used by the HTTPS scheme
/// guard to permit mockito-based test fixtures (which serve
/// `http://127.0.0.1:<port>/`) without weakening production security:
/// real deployments never reach a literal IP here because
/// registration-time validation rejects them.
fn endpoint_is_loopback(endpoint: &Url) -> bool {
    match endpoint.host() {
        Some(url::Host::Ipv4(addr)) => addr.is_loopback(),
        Some(url::Host::Ipv6(addr)) => addr.is_loopback(),
        Some(url::Host::Domain(name)) => {
            let lower = name.to_ascii_lowercase();
            lower == "localhost" || lower.ends_with(".localhost")
        }
        None => false,
    }
}

/// RFC 7231 §7.1.3 `Retry-After` is either a delta-seconds or an
/// HTTP-date. Only the delta-seconds form is honored — HTTP-date
/// parsing would pull in another dep for a value the publish-job
/// worker already clamps against `next_retry_at_ms` policy.
fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::types::{AuthSecret, EncryptedPayload, PushTopic, VapidJwt};
    use std::sync::Arc;

    fn dummy_jwt() -> VapidJwt {
        // Three-segment ASCII string; jsonwebtoken doesn't validate here.
        VapidJwt::new("eyJhbGciOiJFUzI1NiJ9.eyJhdWQiOiJodHRwczovL2V4Lm9yZyJ9.sig").expect("jwt")
    }

    fn _ensure_arc_authsecret_compiles() {
        // Compile-time guard so this file's tests link against the
        // typed `AuthSecret` API rather than reaching for a `&[u8]`.
        let _: Arc<AuthSecret> = Arc::new(AuthSecret::from_bytes([0u8; 16]));
    }

    fn payload() -> EncryptedPayload {
        EncryptedPayload::new(vec![0xAB; 32])
    }

    #[test]
    fn urgency_header_values() {
        assert_eq!(Urgency::VeryLow.as_header(), "very-low");
        assert_eq!(Urgency::Low.as_header(), "low");
        assert_eq!(Urgency::Normal.as_header(), "normal");
        assert_eq!(Urgency::High.as_header(), "high");
    }

    #[test]
    fn parse_retry_after_seconds() {
        assert_eq!(parse_retry_after("30"), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after(" 30 "), Some(Duration::from_secs(30)));
    }

    #[test]
    fn parse_retry_after_rejects_http_date() {
        // We only honor delta-seconds; HTTP-date returns None.
        assert_eq!(parse_retry_after("Fri, 31 Dec 1999 23:59:59 GMT"), None);
    }

    #[tokio::test]
    async fn classify_2xx_is_delivered() {
        let server = mockito::Server::new_async().await;
        let mut server = server;
        let m = server
            .mock("POST", "/p/abc")
            .with_status(201)
            .create_async()
            .await;
        let url = Url::parse(&format!("{}/p/abc", server.url())).unwrap();
        let sender = HttpWebPushSender::new();
        let jwt = dummy_jwt();
        let p = payload();
        let topic = PushTopic::new("d-test").ok();
        let outcome = sender
            .send(WebPushRequest {
                endpoint: &url,
                payload: &p,
                vapid_jwt: &jwt,
                vapid_public_key_b64u: "BFoo",
                topic: topic.as_ref(),
                ttl: 60,
                urgency: Urgency::Normal,
            })
            .await;
        m.assert_async().await;
        assert!(matches!(outcome, WebPushOutcome::Delivered { status: 201 }));
    }

    #[tokio::test]
    async fn classify_410_is_subscription_gone() {
        let server = mockito::Server::new_async().await;
        let mut server = server;
        let m = server
            .mock("POST", "/p/x")
            .with_status(410)
            .create_async()
            .await;
        let url = Url::parse(&format!("{}/p/x", server.url())).unwrap();
        let sender = HttpWebPushSender::new();
        let jwt = dummy_jwt();
        let p = payload();
        let outcome = sender
            .send(WebPushRequest {
                endpoint: &url,
                payload: &p,
                vapid_jwt: &jwt,
                vapid_public_key_b64u: "BFoo",
                topic: None,
                ttl: 60,
                urgency: Urgency::Normal,
            })
            .await;
        m.assert_async().await;
        assert!(matches!(
            outcome,
            WebPushOutcome::SubscriptionGone { status: 410 }
        ));
    }

    #[tokio::test]
    async fn classify_401_with_vapid_www_authenticate_is_clock_skew() {
        // 401 + `WWW-Authenticate: vapid` is the RFC 8292 §3 signal
        // that the JWT itself was rejected — clock skew or expired
        // exp. Worker invalidates the JWT cache on this outcome.
        let server = mockito::Server::new_async().await;
        let mut server = server;
        let m = server
            .mock("POST", "/p/y")
            .with_status(401)
            .with_header("www-authenticate", "vapid")
            .create_async()
            .await;
        let url = Url::parse(&format!("{}/p/y", server.url())).unwrap();
        let sender = HttpWebPushSender::new();
        let jwt = dummy_jwt();
        let p = payload();
        let outcome = sender
            .send(WebPushRequest {
                endpoint: &url,
                payload: &p,
                vapid_jwt: &jwt,
                vapid_public_key_b64u: "BFoo",
                topic: None,
                ttl: 60,
                urgency: Urgency::Normal,
            })
            .await;
        m.assert_async().await;
        assert!(matches!(outcome, WebPushOutcome::ClockSkew { status: 401 }));
    }

    #[tokio::test]
    async fn classify_401_without_vapid_www_authenticate_is_bad_request() {
        // Generic 401 without the vapid challenge — could be relay
        // misconfig or rate-limit-style auth gate. Do NOT invalidate
        // the JWT cache; classify as BadRequest so the operator audit
        // surfaces the cause without thrashing the cache.
        let server = mockito::Server::new_async().await;
        let mut server = server;
        let m = server
            .mock("POST", "/p/y")
            .with_status(401)
            .create_async()
            .await;
        let url = Url::parse(&format!("{}/p/y", server.url())).unwrap();
        let sender = HttpWebPushSender::new();
        let jwt = dummy_jwt();
        let p = payload();
        let outcome = sender
            .send(WebPushRequest {
                endpoint: &url,
                payload: &p,
                vapid_jwt: &jwt,
                vapid_public_key_b64u: "BFoo",
                topic: None,
                ttl: 60,
                urgency: Urgency::Normal,
            })
            .await;
        m.assert_async().await;
        assert!(matches!(
            outcome,
            WebPushOutcome::BadRequest { status: 401 }
        ));
    }

    #[tokio::test]
    async fn classify_413_is_payload_too_large() {
        let server = mockito::Server::new_async().await;
        let mut server = server;
        let m = server
            .mock("POST", "/p/z")
            .with_status(413)
            .create_async()
            .await;
        let url = Url::parse(&format!("{}/p/z", server.url())).unwrap();
        let sender = HttpWebPushSender::new();
        let jwt = dummy_jwt();
        let p = payload();
        let outcome = sender
            .send(WebPushRequest {
                endpoint: &url,
                payload: &p,
                vapid_jwt: &jwt,
                vapid_public_key_b64u: "BFoo",
                topic: None,
                ttl: 60,
                urgency: Urgency::Normal,
            })
            .await;
        m.assert_async().await;
        assert!(matches!(
            outcome,
            WebPushOutcome::PayloadTooLarge { status: 413 }
        ));
    }

    #[tokio::test]
    async fn classify_429_carries_retry_after() {
        let server = mockito::Server::new_async().await;
        let mut server = server;
        let m = server
            .mock("POST", "/p/q")
            .with_status(429)
            .with_header("retry-after", "45")
            .create_async()
            .await;
        let url = Url::parse(&format!("{}/p/q", server.url())).unwrap();
        let sender = HttpWebPushSender::new();
        let jwt = dummy_jwt();
        let p = payload();
        let outcome = sender
            .send(WebPushRequest {
                endpoint: &url,
                payload: &p,
                vapid_jwt: &jwt,
                vapid_public_key_b64u: "BFoo",
                topic: None,
                ttl: 60,
                urgency: Urgency::Normal,
            })
            .await;
        m.assert_async().await;
        match outcome {
            WebPushOutcome::RateLimited {
                status: 429,
                retry_after,
            } => {
                assert_eq!(retry_after, Some(Duration::from_secs(45)));
            }
            other => panic!("expected RateLimited, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn classify_5xx_is_transient_server_error() {
        let server = mockito::Server::new_async().await;
        let mut server = server;
        let m = server
            .mock("POST", "/p/s")
            .with_status(503)
            .create_async()
            .await;
        let url = Url::parse(&format!("{}/p/s", server.url())).unwrap();
        let sender = HttpWebPushSender::new();
        let jwt = dummy_jwt();
        let p = payload();
        let outcome = sender
            .send(WebPushRequest {
                endpoint: &url,
                payload: &p,
                vapid_jwt: &jwt,
                vapid_public_key_b64u: "BFoo",
                topic: None,
                ttl: 60,
                urgency: Urgency::Normal,
            })
            .await;
        m.assert_async().await;
        assert!(matches!(
            outcome,
            WebPushOutcome::Transient {
                kind: TransientFailure::ServerError { status: 503 }
            }
        ));
    }

    #[tokio::test]
    async fn classify_400_is_bad_request() {
        let server = mockito::Server::new_async().await;
        let mut server = server;
        let m = server
            .mock("POST", "/p/b")
            .with_status(400)
            .create_async()
            .await;
        let url = Url::parse(&format!("{}/p/b", server.url())).unwrap();
        let sender = HttpWebPushSender::new();
        let jwt = dummy_jwt();
        let p = payload();
        let outcome = sender
            .send(WebPushRequest {
                endpoint: &url,
                payload: &p,
                vapid_jwt: &jwt,
                vapid_public_key_b64u: "BFoo",
                topic: None,
                ttl: 60,
                urgency: Urgency::Normal,
            })
            .await;
        m.assert_async().await;
        assert!(matches!(
            outcome,
            WebPushOutcome::BadRequest { status: 400 }
        ));
    }

    #[tokio::test]
    async fn classify_403_is_bad_request_not_subscription_gone() {
        // 403 is overloaded by real relays: it covers "VAPID key not
        // authorized for this endpoint" (per-device) AND cluster-wide
        // JWT rejection (wrong aud / expired exp / malformed JWT)
        // which is a deployment bug affecting all devices. Routing
        // every 403 to SubscriptionGone would mass-disable on a
        // config error. Classify as BadRequest (permanent, non-
        // disabling) so the failure surfaces in the operator audit
        // without bricking the user base.
        let server = mockito::Server::new_async().await;
        let mut server = server;
        let m = server
            .mock("POST", "/p/forbidden")
            .with_status(403)
            .create_async()
            .await;
        let url = Url::parse(&format!("{}/p/forbidden", server.url())).unwrap();
        let sender = HttpWebPushSender::new();
        let jwt = dummy_jwt();
        let p = payload();
        let outcome = sender
            .send(WebPushRequest {
                endpoint: &url,
                payload: &p,
                vapid_jwt: &jwt,
                vapid_public_key_b64u: "BFoo",
                topic: None,
                ttl: 60,
                urgency: Urgency::Normal,
            })
            .await;
        m.assert_async().await;
        assert!(matches!(
            outcome,
            WebPushOutcome::BadRequest { status: 403 }
        ));
    }

    #[tokio::test]
    async fn sends_aes128gcm_headers_and_body() {
        let server = mockito::Server::new_async().await;
        let mut server = server;
        let m = server
            .mock("POST", "/p/h")
            .match_header("content-encoding", "aes128gcm")
            .match_header("content-type", "application/octet-stream")
            .match_header("ttl", "120")
            .match_header("urgency", "high")
            .match_header("topic", "d-fingerprint")
            .match_header(
                "authorization",
                "vapid t=eyJhbGciOiJFUzI1NiJ9.eyJhdWQiOiJodHRwczovL2V4Lm9yZyJ9.sig, k=BFoo",
            )
            .with_status(201)
            .create_async()
            .await;
        let url = Url::parse(&format!("{}/p/h", server.url())).unwrap();
        let sender = HttpWebPushSender::new();
        let jwt = dummy_jwt();
        let p = payload();
        let topic = PushTopic::new("d-fingerprint").expect("topic");
        let outcome = sender
            .send(WebPushRequest {
                endpoint: &url,
                payload: &p,
                vapid_jwt: &jwt,
                vapid_public_key_b64u: "BFoo",
                topic: Some(&topic),
                ttl: 120,
                urgency: Urgency::High,
            })
            .await;
        m.assert_async().await;
        assert!(matches!(outcome, WebPushOutcome::Delivered { status: 201 }));
    }
}
