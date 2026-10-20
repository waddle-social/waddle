//! FCM HTTP v1 `messages:send` transport.
//!
//! Like the APNs sender, the transport is narrow: it takes a fully
//! prepared [`FcmRequest`] (validated project id, access token, encoded
//! message) and returns a typed [`FcmOutcome`]. The access token is
//! fetched before the call so the worker reuses one token across the
//! whole device fan-out.
//!
//! The client is its own: HTTPS only, a 10-second budget and no
//! redirects. It is not the APNs client, which speaks HTTP/2 with prior
//! knowledge and so cannot negotiate with Google's front end.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use reqwest::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use thiserror::Error;
use tracing::{debug, warn};
use url::Url;

use super::access_token::FcmAccessToken;
use super::identifiers::FcmProjectId;
use super::message::EncodedFcmMessage;
use super::outcome::{classify_response, FcmErrorDetail, FcmOutcome, FcmTransient};
use crate::push::types::TransientFailure;

/// FCM HTTP v1 API base.
pub const FCM_API_BASE_URL: &str = "https://fcm.googleapis.com/";
/// Per-attempt budget, matching the Web Push and APNs senders.
const FCM_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// FCM error bodies are well under this; anything larger is not
/// Google's JSON shape and is not read further.
const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;

/// All inputs for one `POST /v1/projects/{project}/messages:send`.
#[derive(Debug, Clone, Copy)]
pub struct FcmRequest<'a> {
    pub project_id: &'a FcmProjectId,
    pub access_token: &'a FcmAccessToken,
    pub message: &'a EncodedFcmMessage,
}

/// Transport-layer FCM sender. Object-safe, mirroring
/// [`crate::push::apns::ApnsSender`]; every failure is an [`FcmOutcome`]
/// variant.
pub trait FcmSender: Send + Sync + 'static {
    fn send(
        &self,
        request: FcmRequest<'_>,
    ) -> Pin<Box<dyn Future<Output = FcmOutcome> + Send + '_>>;
}

/// Why the HTTP client could not be built.
#[derive(Debug, Error)]
pub enum FcmSenderBuildError {
    #[error("failed to build the FCM HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("FCM base URL is invalid")]
    BaseUrl,
}

/// reqwest-backed FCM sender.
#[derive(Debug, Clone)]
pub struct HttpFcmSender {
    client: reqwest::Client,
    /// Test-only override of the API base (a local mock server).
    /// Release builds have no such field: every send addresses
    /// [`FCM_API_BASE_URL`] directly.
    #[cfg(test)]
    base: Url,
}

impl HttpFcmSender {
    /// Sender for Google's production FCM endpoint, HTTPS only.
    pub fn new() -> Result<Self, FcmSenderBuildError> {
        let client = base_client_builder()
            .https_only(true)
            .build()
            .map_err(FcmSenderBuildError::Client)?;
        // The base is a constant; parsing it here proves at boot that
        // every send can address it.
        Url::parse(FCM_API_BASE_URL).map_err(|_| FcmSenderBuildError::BaseUrl)?;
        Ok(Self {
            client,
            #[cfg(test)]
            base: Url::parse(FCM_API_BASE_URL).map_err(|_| FcmSenderBuildError::BaseUrl)?,
        })
    }

    /// Test-only sender pointed at a local mock server (cleartext).
    /// Never compiled into a release binary.
    #[cfg(test)]
    fn with_base_url(base: Url) -> Result<Self, FcmSenderBuildError> {
        let client = base_client_builder()
            .build()
            .map_err(FcmSenderBuildError::Client)?;
        Ok(Self { client, base })
    }

    fn send_url(&self, project_id: &FcmProjectId) -> Option<Url> {
        let mut url = self.api_base()?;
        url.path_segments_mut().ok()?.clear().extend([
            "v1",
            "projects",
            project_id.as_str(),
            "messages:send",
        ]);
        Some(url)
    }
}

impl HttpFcmSender {
    /// The API base: always Google's FCM endpoint in release builds.
    #[cfg(not(test))]
    fn api_base(&self) -> Option<Url> {
        Url::parse(FCM_API_BASE_URL).ok()
    }

    #[cfg(test)]
    fn api_base(&self) -> Option<Url> {
        Some(self.base.clone())
    }
}

fn base_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(FCM_REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
}

/// A request that could not be built locally (no HTTP exchange took
/// place). Permanent for this message; the device is kept.
fn preflight_rejection() -> FcmOutcome {
    FcmOutcome::Rejected {
        status: 0,
        detail: FcmErrorDetail::default(),
    }
}

impl FcmSender for HttpFcmSender {
    fn send(
        &self,
        request: FcmRequest<'_>,
    ) -> Pin<Box<dyn Future<Output = FcmOutcome> + Send + '_>> {
        let Some(url) = self.send_url(request.project_id) else {
            return Box::pin(async { preflight_rejection() });
        };
        let bearer = format!("Bearer {}", request.access_token.as_str());
        let Ok(authorization) = HeaderValue::from_str(&bearer) else {
            warn!("FCM request refused: access token is not a visible-ASCII header value");
            return Box::pin(async { preflight_rejection() });
        };
        let builder = self
            .client
            .post(url)
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(request.message.as_slice().to_vec());
        Box::pin(async move {
            match builder.send().await {
                Ok(response) => read_response(response).await,
                Err(error) => classify_transport_error(error),
            }
        })
    }
}

async fn read_response(response: reqwest::Response) -> FcmOutcome {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(crate::push::sender::parse_retry_after);
    let detail = if (200..300).contains(&status) {
        FcmErrorDetail::default()
    } else {
        FcmErrorDetail::from_body(&read_body_bounded(response).await)
    };
    let outcome = classify_response(status, detail, retry_after);
    debug!(status, detail = %detail, "FCM response");
    outcome
}

/// Read at most [`MAX_ERROR_BODY_BYTES`]; a longer body is not Google's
/// JSON shape and parses as an empty detail.
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

fn classify_transport_error(error: reqwest::Error) -> FcmOutcome {
    let failure = TransientFailure::from_transport_error(&error);
    warn!(
        error = %error.without_url(),
        timeout = matches!(failure, TransientFailure::Timeout),
        "FCM transport failure"
    );
    FcmOutcome::Transient {
        cause: FcmTransient::Failure(failure),
        retry_after: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::fcm::identifiers::FcmRegistrationToken;
    use crate::push::fcm::message::{FcmMessage, FcmPriority, FcmTtl};
    use crate::push::fcm::outcome::{FcmErrorCode, GoogleRpcStatus};
    use crate::push::types::EncryptedPayload;

    const PROJECT: &str = "waddle-test";
    const SEND_PATH: &str = "/v1/projects/waddle-test/messages:send";

    struct Fixture {
        project: FcmProjectId,
        token: FcmAccessToken,
        message: EncodedFcmMessage,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                project: FcmProjectId::parse(PROJECT).expect("project"),
                token: FcmAccessToken::new("ya29.test-token".to_string()),
                message: FcmMessage::new(
                    FcmRegistrationToken::parse("device-token").expect("token"),
                    &EncryptedPayload::new(vec![7; 64]),
                    FcmPriority::High,
                    FcmTtl::new(Duration::from_secs(3600)),
                )
                .encode()
                .expect("message"),
            }
        }

        fn request(&self) -> FcmRequest<'_> {
            FcmRequest {
                project_id: &self.project,
                access_token: &self.token,
                message: &self.message,
            }
        }
    }

    fn sender_for(server: &mockito::Server) -> HttpFcmSender {
        HttpFcmSender::with_base_url(Url::parse(&server.url()).expect("mock url")).expect("sender")
    }

    async fn send_with(status: usize, body: &str, retry_after: Option<&str>) -> FcmOutcome {
        let mut server = mockito::Server::new_async().await;
        let mut mock = server
            .mock("POST", SEND_PATH)
            .with_status(status)
            .with_body(body);
        if let Some(value) = retry_after {
            mock = mock.with_header("retry-after", value);
        }
        let mock = mock.create_async().await;
        let fixture = Fixture::new();
        let outcome = sender_for(&server).send(fixture.request()).await;
        mock.assert_async().await;
        outcome
    }

    fn fcm_error_body(code: u16, status: &str, error_code: &str) -> String {
        serde_json::json!({
            "error": {
                "code": code,
                "message": "Requested entity was not found.",
                "status": status,
                "details": [{
                    "@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError",
                    "errorCode": error_code,
                }],
            }
        })
        .to_string()
    }

    #[test]
    fn production_sender_targets_the_fcm_v1_endpoint() {
        let sender = HttpFcmSender::new().expect("sender");
        let project = FcmProjectId::parse("waddle-social").expect("project");
        assert_eq!(
            sender.send_url(&project).expect("url").as_str(),
            "https://fcm.googleapis.com/v1/projects/waddle-social/messages:send"
        );
    }

    #[tokio::test]
    async fn sends_bearer_token_and_json_body() {
        let mut server = mockito::Server::new_async().await;
        let fixture = Fixture::new();
        let mock = server
            .mock("POST", SEND_PATH)
            .match_header("authorization", "Bearer ya29.test-token")
            .match_header("content-type", "application/json")
            .match_body(mockito::Matcher::Exact(
                String::from_utf8(fixture.message.as_slice().to_vec()).expect("utf-8"),
            ))
            .with_status(200)
            .with_body(r#"{"name":"projects/waddle-test/messages/0:1"}"#)
            .create_async()
            .await;
        let outcome = sender_for(&server).send(fixture.request()).await;
        mock.assert_async().await;
        assert_eq!(outcome, FcmOutcome::Sent);
    }

    #[tokio::test]
    async fn unregistered_is_classified_from_the_body() {
        let outcome = send_with(404, &fcm_error_body(404, "NOT_FOUND", "UNREGISTERED"), None).await;
        assert!(matches!(
            outcome,
            FcmOutcome::Unregistered {
                status: 404,
                detail: FcmErrorDetail {
                    status: Some(GoogleRpcStatus::NotFound),
                    error_code: Some(FcmErrorCode::Unregistered),
                    ..
                }
            }
        ));
    }

    #[tokio::test]
    async fn bare_404_is_provider_auth_not_device_gone() {
        assert!(matches!(
            send_with(404, "", None).await,
            FcmOutcome::ProviderAuth { status: 404, .. }
        ));
    }

    #[tokio::test]
    async fn bare_403_is_provider_auth() {
        assert!(matches!(
            send_with(403, "<html>forbidden</html>", None).await,
            FcmOutcome::ProviderAuth { status: 403, .. }
        ));
    }

    #[tokio::test]
    async fn unauthenticated_is_reported_for_a_token_refresh() {
        assert!(matches!(
            send_with(401, "", None).await,
            FcmOutcome::Unauthenticated { .. }
        ));
    }

    #[tokio::test]
    async fn quota_exceeded_is_rate_limited_with_retry_after() {
        assert_eq!(
            send_with(
                429,
                &fcm_error_body(429, "RESOURCE_EXHAUSTED", "QUOTA_EXCEEDED"),
                Some("120")
            )
            .await,
            FcmOutcome::Transient {
                cause: FcmTransient::RateLimited,
                retry_after: Some(Duration::from_secs(120)),
            }
        );
    }

    #[tokio::test]
    async fn unavailable_is_transient() {
        assert_eq!(
            send_with(
                503,
                &fcm_error_body(503, "UNAVAILABLE", "UNAVAILABLE"),
                None
            )
            .await,
            FcmOutcome::Transient {
                cause: FcmTransient::Unavailable,
                retry_after: None,
            }
        );
    }

    #[tokio::test]
    async fn established_connection_timeout_keeps_acceptance_unknown() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let endpoint = Url::parse(&format!(
            "http://{}/",
            listener.local_addr().expect("address")
        ))
        .expect("url");
        let (accepted, established) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (_connection, _) = listener.accept().await.expect("accept connection");
            accepted.send(()).expect("test awaiting setup");
            std::future::pending::<()>().await;
        });
        let mut sender = HttpFcmSender::with_base_url(endpoint).expect("sender");
        sender.client = base_client_builder()
            .timeout(Duration::from_millis(100))
            .no_proxy()
            .build()
            .expect("client");
        let fixture = Fixture::new();
        let outcome = sender.send(fixture.request()).await;
        established.await.expect("TCP connection was established");
        server.abort();
        assert_eq!(
            outcome,
            FcmOutcome::Transient {
                cause: FcmTransient::Failure(TransientFailure::Timeout),
                retry_after: None
            }
        );
    }

    #[tokio::test]
    async fn connection_refused_is_pre_send_connect_failure() {
        let sender = HttpFcmSender::with_base_url(Url::parse("http://127.0.0.1:1/").expect("url"))
            .expect("sender");
        let fixture = Fixture::new();
        assert_eq!(
            sender.send(fixture.request()).await,
            FcmOutcome::Transient {
                cause: FcmTransient::Failure(TransientFailure::Connect),
                retry_after: None,
            }
        );
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", SEND_PATH)
            .with_status(307)
            .with_header("location", "https://elsewhere.example/steal")
            .create_async()
            .await;
        let fixture = Fixture::new();
        let outcome = sender_for(&server).send(fixture.request()).await;
        mock.assert_async().await;
        assert!(matches!(outcome, FcmOutcome::Rejected { status: 307, .. }));
    }
}
