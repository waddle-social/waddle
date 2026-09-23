//! APNs HTTP/2 provider API transport.
//!
//! Like the Web Push sender, the transport is narrow: it takes a fully
//! prepared [`ApnsRequest`] (validated token, topic, signed provider
//! token, encoded payload) and returns a typed [`ApnsOutcome`]. Token
//! signing and payload building happen before the call, so the worker
//! reuses one provider token across the whole device fan-out.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue, AUTHORIZATION, RETRY_AFTER};
use thiserror::Error;
use tracing::{debug, warn};
use url::Url;

use super::environment::ApnsEnvironment;
use super::identifiers::{ApnsCollapseId, ApnsDeviceToken, ApnsTopic};
use super::outcome::{classify_response, ApnsId, ApnsOutcome, ApnsReason, ApnsTransient};
use super::payload::EncodedApnsPayload;
use super::provider_token::ApnsProviderJwt;
use crate::push::types::TransientFailure;

const APNS_TOPIC: HeaderName = HeaderName::from_static("apns-topic");
const APNS_PUSH_TYPE: HeaderName = HeaderName::from_static("apns-push-type");
const APNS_PRIORITY: HeaderName = HeaderName::from_static("apns-priority");
const APNS_EXPIRATION: HeaderName = HeaderName::from_static("apns-expiration");
const APNS_COLLAPSE_ID: HeaderName = HeaderName::from_static("apns-collapse-id");
const APNS_ID: HeaderName = HeaderName::from_static("apns-id");
/// Every Waddle APNs push is a user-visible alert.
const PUSH_TYPE_ALERT: &str = "alert";
/// Per-attempt budget, matching the Web Push sender.
const APNS_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// APNs error bodies are a few dozen bytes of JSON; anything larger is
/// not Apple's shape and is not read further.
const MAX_ERROR_BODY_BYTES: usize = 4096;

/// `apns-priority`: `10` delivers immediately, `5` lets the device
/// batch delivery to save power.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApnsPriority {
    Immediate,
    PowerConsiderate,
}

impl ApnsPriority {
    fn as_header(self) -> HeaderValue {
        match self {
            Self::Immediate => HeaderValue::from_static("10"),
            Self::PowerConsiderate => HeaderValue::from_static("5"),
        }
    }
}

/// `apns-expiration`: UNIX seconds after which APNs stops trying to
/// deliver to an offline device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApnsExpiration(u64);

impl ApnsExpiration {
    /// `now + ttl`, saturating.
    pub fn after(now_unix_seconds: u64, ttl: Duration) -> Self {
        Self(now_unix_seconds.saturating_add(ttl.as_secs()))
    }

    pub fn unix_seconds(self) -> u64 {
        self.0
    }
}

/// All inputs for one `POST /3/device/<token>`.
#[derive(Debug, Clone, Copy)]
pub struct ApnsRequest<'a> {
    pub environment: ApnsEnvironment,
    pub device_token: &'a ApnsDeviceToken,
    pub topic: &'a ApnsTopic,
    pub provider_token: &'a ApnsProviderJwt,
    pub payload: &'a EncodedApnsPayload,
    pub priority: ApnsPriority,
    pub expiration: ApnsExpiration,
    pub collapse_id: Option<&'a ApnsCollapseId>,
}

/// Transport-layer APNs sender. Object-safe, mirroring
/// [`crate::push::WebPushSender`]; every failure is an [`ApnsOutcome`]
/// variant.
pub trait ApnsSender: Send + Sync + 'static {
    fn send(
        &self,
        request: ApnsRequest<'_>,
    ) -> Pin<Box<dyn Future<Output = ApnsOutcome> + Send + '_>>;
}

/// Why the HTTP client could not be built.
#[derive(Debug, Error)]
pub enum ApnsSenderBuildError {
    #[error("failed to build the APNs HTTP/2 client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("APNs base URL is invalid")]
    BaseUrl,
}

/// reqwest-backed APNs sender. APNs only speaks HTTP/2, so the client
/// uses HTTP/2 exclusively (`h2` is the only ALPN protocol offered over
/// TLS).
#[derive(Debug, Clone)]
pub struct HttpApnsSender {
    client: reqwest::Client,
    production: Url,
    sandbox: Url,
}

impl HttpApnsSender {
    /// Sender for Apple's production and sandbox hosts, HTTPS only.
    pub fn new() -> Result<Self, ApnsSenderBuildError> {
        let client = base_client_builder()
            .https_only(true)
            .build()
            .map_err(ApnsSenderBuildError::Client)?;
        Ok(Self {
            client,
            production: host_base_url(ApnsEnvironment::Production)?,
            sandbox: host_base_url(ApnsEnvironment::Sandbox)?,
        })
    }

    /// Test-only sender pointed at local mock servers (cleartext
    /// HTTP/2). Never compiled into a release binary.
    #[cfg(test)]
    fn with_base_urls(production: Url, sandbox: Url) -> Result<Self, ApnsSenderBuildError> {
        let client = base_client_builder()
            .build()
            .map_err(ApnsSenderBuildError::Client)?;
        Ok(Self {
            client,
            production,
            sandbox,
        })
    }

    fn device_url(&self, environment: ApnsEnvironment, token: &ApnsDeviceToken) -> Option<Url> {
        let mut url = match environment {
            ApnsEnvironment::Production => self.production.clone(),
            ApnsEnvironment::Sandbox => self.sandbox.clone(),
        };
        url.path_segments_mut()
            .ok()?
            .clear()
            .extend(["3", "device", token.as_str()]);
        Some(url)
    }
}

fn base_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .http2_prior_knowledge()
        .timeout(APNS_REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
}

fn host_base_url(environment: ApnsEnvironment) -> Result<Url, ApnsSenderBuildError> {
    let mut url = Url::parse("https://localhost/").map_err(|_| ApnsSenderBuildError::BaseUrl)?;
    url.set_host(Some(environment.host()))
        .map_err(|_| ApnsSenderBuildError::BaseUrl)?;
    Ok(url)
}

/// A request that could not be built locally (no HTTP exchange took
/// place). Permanent for this job; the device is kept.
fn preflight_rejection() -> ApnsOutcome {
    ApnsOutcome::Rejected {
        status: 0,
        reason: ApnsReason::Unrecognized,
    }
}

impl ApnsSender for HttpApnsSender {
    fn send(
        &self,
        request: ApnsRequest<'_>,
    ) -> Pin<Box<dyn Future<Output = ApnsOutcome> + Send + '_>> {
        let environment = request.environment;
        let Some(url) = self.device_url(environment, request.device_token) else {
            return Box::pin(async { preflight_rejection() });
        };
        let bearer = format!("bearer {}", request.provider_token.as_str());
        let (Ok(authorization), Ok(topic)) = (
            HeaderValue::from_str(&bearer),
            HeaderValue::from_str(request.topic.as_str()),
        ) else {
            warn!(
                environment = environment.host(),
                "APNs request refused: header value is not visible ASCII"
            );
            return Box::pin(async { preflight_rejection() });
        };
        let mut builder = self
            .client
            .post(url)
            .header(AUTHORIZATION, authorization)
            .header(APNS_TOPIC, topic)
            .header(APNS_PUSH_TYPE, HeaderValue::from_static(PUSH_TYPE_ALERT))
            .header(APNS_PRIORITY, request.priority.as_header())
            .header(
                APNS_EXPIRATION,
                HeaderValue::from(request.expiration.unix_seconds()),
            )
            .body(request.payload.as_slice().to_vec());
        if let Some(collapse_id) = request
            .collapse_id
            .and_then(|id| HeaderValue::from_str(id.as_str()).ok())
        {
            builder = builder.header(APNS_COLLAPSE_ID, collapse_id);
        }
        Box::pin(async move {
            match builder.send().await {
                Ok(response) => read_response(environment, response).await,
                Err(error) => classify_transport_error(environment, &error),
            }
        })
    }
}

async fn read_response(environment: ApnsEnvironment, response: reqwest::Response) -> ApnsOutcome {
    let status = response.status().as_u16();
    let apns_id = response
        .headers()
        .get(APNS_ID)
        .and_then(|value| value.to_str().ok())
        .and_then(ApnsId::parse);
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(crate::push::sender::parse_retry_after);
    let reason = if status == 200 {
        ApnsReason::Unrecognized
    } else {
        ApnsReason::from_body(&read_body_bounded(response).await)
    };
    let outcome = classify_response(status, reason, retry_after, apns_id);
    debug!(
        environment = environment.host(),
        status,
        reason = reason.as_str(),
        "APNs response"
    );
    outcome
}

/// Read at most [`MAX_ERROR_BODY_BYTES`]; a longer body is not Apple's
/// JSON shape and parses as [`ApnsReason::Unrecognized`].
async fn read_body_bounded(mut response: reqwest::Response) -> Vec<u8> {
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        if body.len() + chunk.len() > MAX_ERROR_BODY_BYTES {
            return Vec::new();
        }
        body.extend_from_slice(&chunk);
    }
    body
}

fn classify_transport_error(environment: ApnsEnvironment, error: &reqwest::Error) -> ApnsOutcome {
    let failure = if error.is_timeout() {
        TransientFailure::Timeout
    } else {
        TransientFailure::Network
    };
    warn!(
        environment = environment.host(),
        error = %error,
        timeout = error.is_timeout(),
        "APNs transport failure"
    );
    ApnsOutcome::Transient {
        cause: ApnsTransient::Failure(failure),
        retry_after: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::apns::identifiers::ApnsTopic;
    use crate::push::apns::outcome::ApnsTransient;
    use crate::push::apns::payload::{ApnsPayload, ApnsPayloadFields};
    use crate::push::apns::provider_token::{
        ApnsClock, ApnsProviderTokenSource, CachingApnsTokenSigner,
    };
    use crate::push::apns::{ApnsKeyId, ApnsTeamId};
    use crate::push::envelope::NotificationClass;
    use p256::elliptic_curve::rand_core::OsRng;
    use p256::pkcs8::EncodePrivateKey;
    use std::sync::Arc;

    const TOKEN: &str = "0a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f9";
    const APNS_ID_VALUE: &str = "8c8b2a84-7c8e-4a8f-9a0e-1c1f5d8e2b3a";

    struct FixedClock;

    impl ApnsClock for FixedClock {
        fn now_unix_seconds(&self) -> u64 {
            1_700_000_000
        }
    }

    struct Fixture {
        token: ApnsDeviceToken,
        topic: ApnsTopic,
        jwt: ApnsProviderJwt,
        payload: EncodedApnsPayload,
        collapse_id: ApnsCollapseId,
    }

    impl Fixture {
        fn new() -> Self {
            let pem = p256::SecretKey::random(&mut OsRng)
                .to_pkcs8_pem(Default::default())
                .expect("pem");
            let signer = CachingApnsTokenSigner::from_pkcs8_pem(
                ApnsTeamId::parse("TEAM123456").expect("team"),
                ApnsKeyId::parse("KEY1234567").expect("key"),
                &pem,
                Arc::new(FixedClock),
            )
            .expect("signer");
            let payload = ApnsPayload::new(ApnsPayloadFields {
                class: NotificationClass::Dm,
                conversation: "alice@example.com",
                thread: None,
                item: "stanza-1",
                node: "node-1",
                message_count: Some(2),
            })
            .encode()
            .expect("payload");
            Self {
                token: ApnsDeviceToken::parse(TOKEN).expect("token"),
                topic: ApnsTopic::parse("social.waddle.app").expect("topic"),
                jwt: signer.current().expect("jwt"),
                payload,
                collapse_id: ApnsCollapseId::new("stanza-1").expect("collapse id"),
            }
        }

        fn request(&self, environment: ApnsEnvironment) -> ApnsRequest<'_> {
            ApnsRequest {
                environment,
                device_token: &self.token,
                topic: &self.topic,
                provider_token: &self.jwt,
                payload: &self.payload,
                priority: ApnsPriority::Immediate,
                expiration: ApnsExpiration::after(1_700_000_000, Duration::from_secs(86_400)),
                collapse_id: Some(&self.collapse_id),
            }
        }
    }

    fn sender_for(server: &mockito::Server) -> HttpApnsSender {
        let base = Url::parse(&server.url()).expect("mock url");
        HttpApnsSender::with_base_urls(base.clone(), base).expect("sender")
    }

    async fn send_with_status(status: usize, body: &str, retry_after: Option<&str>) -> ApnsOutcome {
        let mut server = mockito::Server::new_async().await;
        let mut mock = server
            .mock("POST", format!("/3/device/{TOKEN}").as_str())
            .with_status(status)
            .with_body(body);
        if let Some(value) = retry_after {
            mock = mock.with_header("retry-after", value);
        }
        let mock = mock.create_async().await;
        let fixture = Fixture::new();
        let outcome = sender_for(&server)
            .send(fixture.request(ApnsEnvironment::Production))
            .await;
        mock.assert_async().await;
        outcome
    }

    #[test]
    fn production_sender_targets_apple_hosts() {
        let sender = HttpApnsSender::new().expect("sender");
        let token = ApnsDeviceToken::parse(TOKEN).expect("token");
        assert_eq!(
            sender
                .device_url(ApnsEnvironment::Production, &token)
                .expect("url")
                .as_str(),
            format!("https://api.push.apple.com/3/device/{TOKEN}")
        );
        assert_eq!(
            sender
                .device_url(ApnsEnvironment::Sandbox, &token)
                .expect("url")
                .as_str(),
            format!("https://api.sandbox.push.apple.com/3/device/{TOKEN}")
        );
    }

    #[tokio::test]
    async fn sends_apple_headers_and_payload_over_http2() {
        let mut server = mockito::Server::new_async().await;
        let fixture = Fixture::new();
        let mock = server
            .mock("POST", format!("/3/device/{TOKEN}").as_str())
            .match_header(
                "authorization",
                format!("bearer {}", fixture.jwt.as_str()).as_str(),
            )
            .match_header("apns-topic", "social.waddle.app")
            .match_header("apns-push-type", "alert")
            .match_header("apns-priority", "10")
            .match_header("apns-expiration", "1700086400")
            .match_header("apns-collapse-id", "stanza-1")
            .match_body(mockito::Matcher::Exact(
                String::from_utf8(fixture.payload.as_slice().to_vec()).expect("utf-8"),
            ))
            .with_status(200)
            .with_header("apns-id", APNS_ID_VALUE)
            .create_async()
            .await;
        let outcome = sender_for(&server)
            .send(fixture.request(ApnsEnvironment::Production))
            .await;
        mock.assert_async().await;
        assert_eq!(
            outcome,
            ApnsOutcome::Sent {
                apns_id: ApnsId::parse(APNS_ID_VALUE)
            }
        );
    }

    #[tokio::test]
    async fn sandbox_devices_go_to_the_sandbox_base() {
        let mut production = mockito::Server::new_async().await;
        let mut sandbox = mockito::Server::new_async().await;
        let production_mock = production
            .mock("POST", mockito::Matcher::Any)
            .expect(0)
            .create_async()
            .await;
        let sandbox_mock = sandbox
            .mock("POST", format!("/3/device/{TOKEN}").as_str())
            .with_status(200)
            .create_async()
            .await;
        let sender = HttpApnsSender::with_base_urls(
            Url::parse(&production.url()).expect("url"),
            Url::parse(&sandbox.url()).expect("url"),
        )
        .expect("sender");
        let fixture = Fixture::new();
        let outcome = sender.send(fixture.request(ApnsEnvironment::Sandbox)).await;
        sandbox_mock.assert_async().await;
        production_mock.assert_async().await;
        assert!(matches!(outcome, ApnsOutcome::Sent { .. }));
    }

    #[tokio::test]
    async fn bad_device_token_is_device_gone() {
        assert_eq!(
            send_with_status(400, r#"{"reason":"BadDeviceToken"}"#, None).await,
            ApnsOutcome::DeviceGone {
                status: 400,
                reason: ApnsReason::BadDeviceToken
            }
        );
    }

    #[tokio::test]
    async fn device_token_not_for_topic_is_device_gone() {
        assert_eq!(
            send_with_status(400, r#"{"reason":"DeviceTokenNotForTopic"}"#, None).await,
            ApnsOutcome::DeviceGone {
                status: 400,
                reason: ApnsReason::DeviceTokenNotForTopic
            }
        );
    }

    #[tokio::test]
    async fn unregistered_is_device_gone() {
        assert_eq!(
            send_with_status(
                410,
                r#"{"reason":"Unregistered","timestamp":1700000000000}"#,
                None
            )
            .await,
            ApnsOutcome::DeviceGone {
                status: 410,
                reason: ApnsReason::Unregistered
            }
        );
    }

    #[tokio::test]
    async fn expired_provider_token_is_provider_auth() {
        assert_eq!(
            send_with_status(403, r#"{"reason":"ExpiredProviderToken"}"#, None).await,
            ApnsOutcome::ProviderAuth {
                reason: ApnsReason::ExpiredProviderToken
            }
        );
    }

    #[tokio::test]
    async fn payload_too_large_is_rejected_not_gone() {
        assert_eq!(
            send_with_status(413, r#"{"reason":"PayloadTooLarge"}"#, None).await,
            ApnsOutcome::Rejected {
                status: 413,
                reason: ApnsReason::PayloadTooLarge
            }
        );
    }

    #[tokio::test]
    async fn too_many_requests_is_transient_with_retry_after() {
        assert_eq!(
            send_with_status(429, r#"{"reason":"TooManyRequests"}"#, Some("120")).await,
            ApnsOutcome::Transient {
                cause: ApnsTransient::RateLimited {
                    reason: ApnsReason::TooManyRequests
                },
                retry_after: Some(Duration::from_secs(120)),
            }
        );
    }

    #[tokio::test]
    async fn internal_server_error_is_transient() {
        assert_eq!(
            send_with_status(500, r#"{"reason":"InternalServerError"}"#, None).await,
            ApnsOutcome::Transient {
                cause: ApnsTransient::Failure(TransientFailure::ServerError { status: 500 }),
                retry_after: None,
            }
        );
    }

    #[tokio::test]
    async fn service_unavailable_is_transient() {
        assert_eq!(
            send_with_status(503, r#"{"reason":"ServiceUnavailable"}"#, None).await,
            ApnsOutcome::Transient {
                cause: ApnsTransient::Failure(TransientFailure::ServerError { status: 503 }),
                retry_after: None,
            }
        );
    }

    #[tokio::test]
    async fn connection_refused_is_transient_network() {
        let unused = Url::parse("http://127.0.0.1:1/").expect("url");
        let sender = HttpApnsSender::with_base_urls(unused.clone(), unused).expect("sender");
        let fixture = Fixture::new();
        let outcome = sender
            .send(fixture.request(ApnsEnvironment::Production))
            .await;
        assert_eq!(
            outcome,
            ApnsOutcome::Transient {
                cause: ApnsTransient::Failure(TransientFailure::Network),
                retry_after: None,
            }
        );
    }
}
