//! OAuth 2.0 access tokens for the FCM HTTP v1 API.
//!
//! FCM authenticates senders with a short-lived Google OAuth access
//! token, obtained with the JWT-bearer grant (RFC 7523): the server signs
//! an RS256 assertion with its service-account key and exchanges it at
//! Google's token endpoint for a bearer token with the
//! `firebase.messaging` scope.
//!
//! [`OAuthFcmTokenSource`] caches one token until five minutes before
//! it expires and refreshes single-flight: concurrent callers that find
//! the cache stale wait on one exchange instead of each minting their
//! own. The token endpoint is pinned to [`GOOGLE_OAUTH_TOKEN_URL`]; the
//! `token_uri` field of the key file is ignored so a tampered key file
//! cannot redirect the signed assertion elsewhere.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dashmap::{RwLock, RwLockWriteGuard};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::header::{HeaderValue, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;
use url::Url;

use crate::push::types::TransientFailure;

/// Google's OAuth 2.0 token endpoint, also the assertion's `aud`.
pub const GOOGLE_OAUTH_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// The only scope FCM's `messages:send` needs.
pub const FCM_OAUTH_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
/// A cached token is replaced this long before Google says it expires,
/// so a token is never sent moments before it lapses.
pub const FCM_ACCESS_TOKEN_REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);
/// Lifetime of the signed assertion (Google's maximum).
const ASSERTION_LIFETIME: Duration = Duration::from_secs(60 * 60);
/// RFC 7523 grant type.
const JWT_BEARER_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
/// The `type` of a Google service-account key file.
const SERVICE_ACCOUNT_KEY_TYPE: &str = "service_account";
/// Per-request budget for the token exchange.
const TOKEN_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound on the whole single-flight refresh, including waiting for
/// the response body.
const TOKEN_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
/// Token responses are a few hundred bytes of JSON.
const MAX_TOKEN_RESPONSE_BYTES: usize = 16 * 1024;

/// Wall-clock source for assertion `iat` and cache age. Injected so
/// tests can move time without sleeping.
pub trait FcmClock: Send + Sync + 'static {
    fn now_unix_seconds(&self) -> u64;
}

/// Production clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemFcmClock;

impl FcmClock for SystemFcmClock {
    fn now_unix_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0)
    }
}

/// A Google OAuth access token, sent as `authorization: Bearer <token>`.
#[derive(Clone, PartialEq, Eq)]
pub struct FcmAccessToken(String);

impl FcmAccessToken {
    pub fn new(token: String) -> Self {
        Self(token)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The token authorizes sends to every device of the project: never
/// print it.
impl std::fmt::Debug for FcmAccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FcmAccessToken(<redacted>)")
    }
}

/// Why a service-account key file was rejected.
#[derive(Debug, Error)]
pub enum FcmServiceAccountError {
    #[error("FCM service-account key is not valid JSON of the expected shape")]
    NotJson,
    #[error("FCM service-account key has type {0:?}, expected \"service_account\"")]
    WrongType(String),
    #[error("FCM service-account key field {0} is missing or empty")]
    MissingField(&'static str),
    #[error("FCM service-account private key is not a usable RSA key: {0}")]
    PrivateKey(#[source] jsonwebtoken::errors::Error),
}

#[derive(Deserialize)]
struct RawServiceAccountKey {
    #[serde(rename = "type", default)]
    key_type: String,
    #[serde(default)]
    client_email: String,
    #[serde(default)]
    private_key_id: Option<String>,
    #[serde(default)]
    private_key: String,
}

/// A parsed Google service-account key: the identity (`client_email`)
/// and the RS256 signing key. `token_uri` is deliberately not read.
pub struct FcmServiceAccountKey {
    client_email: String,
    private_key_id: Option<String>,
    encoding_key: EncodingKey,
}

impl FcmServiceAccountKey {
    /// Parse the JSON key file Google issues for a service account. The
    /// RSA key is exercised once by signing a probe assertion, so an
    /// unusable key fails here (at boot) rather than on the first send.
    pub fn from_json(json: &[u8]) -> Result<Self, FcmServiceAccountError> {
        let raw: RawServiceAccountKey =
            serde_json::from_slice(json).map_err(|_| FcmServiceAccountError::NotJson)?;
        let private_key = zeroize::Zeroizing::new(raw.private_key);
        if raw.key_type != SERVICE_ACCOUNT_KEY_TYPE {
            return Err(FcmServiceAccountError::WrongType(raw.key_type));
        }
        if raw.client_email.is_empty() {
            return Err(FcmServiceAccountError::MissingField("client_email"));
        }
        if private_key.is_empty() {
            return Err(FcmServiceAccountError::MissingField("private_key"));
        }
        let encoding_key = EncodingKey::from_rsa_pem(private_key.as_bytes())
            .map_err(FcmServiceAccountError::PrivateKey)?;
        let key = Self {
            client_email: raw.client_email,
            private_key_id: raw.private_key_id.filter(|id| !id.is_empty()),
            encoding_key,
        };
        key.sign_assertion(SystemFcmClock.now_unix_seconds())
            .map_err(FcmServiceAccountError::PrivateKey)?;
        Ok(key)
    }

    pub fn client_email(&self) -> &str {
        &self.client_email
    }

    fn sign_assertion(&self, issued_at: u64) -> Result<String, jsonwebtoken::errors::Error> {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = self.private_key_id.clone();
        let claims = AssertionClaims {
            iss: &self.client_email,
            scope: FCM_OAUTH_SCOPE,
            aud: GOOGLE_OAUTH_TOKEN_URL,
            iat: issued_at,
            exp: issued_at.saturating_add(ASSERTION_LIFETIME.as_secs()),
        };
        jsonwebtoken::encode(&header, &claims, &self.encoding_key)
    }
}

impl std::fmt::Debug for FcmServiceAccountKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FcmServiceAccountKey")
            .field("client_email", &self.client_email)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct AssertionClaims<'a> {
    iss: &'a str,
    scope: &'static str,
    aud: &'static str,
    iat: u64,
    exp: u64,
}

/// RFC 6749 §5.2 `error` codes of a refused token request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthErrorCode {
    InvalidRequest,
    InvalidClient,
    InvalidGrant,
    UnauthorizedClient,
    UnsupportedGrantType,
    InvalidScope,
    #[serde(other)]
    Unrecognized,
}

impl OAuthErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::InvalidClient => "invalid_client",
            Self::InvalidGrant => "invalid_grant",
            Self::UnauthorizedClient => "unauthorized_client",
            Self::UnsupportedGrantType => "unsupported_grant_type",
            Self::InvalidScope => "invalid_scope",
            Self::Unrecognized => "unrecognized",
        }
    }

    fn from_body(body: &[u8]) -> Self {
        #[derive(Deserialize)]
        struct ErrorBody {
            error: OAuthErrorCode,
        }
        serde_json::from_slice::<ErrorBody>(body)
            .map(|parsed| parsed.error)
            .unwrap_or(Self::Unrecognized)
    }
}

/// Why no access token could be obtained. FCM was not contacted in any
/// of these cases.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FcmTokenError {
    /// Network error, timeout, `429` or a 5xx from the token endpoint.
    #[error("OAuth token endpoint unreachable: {0:?}")]
    Transient(TransientFailure),
    /// `200` without a usable `access_token`.
    #[error("OAuth token endpoint returned an unusable response")]
    MalformedResponse,
    /// Google refused the assertion (`invalid_grant`, a disabled or
    /// deleted key, a wrong service account): an operator must fix the
    /// credentials.
    #[error("OAuth token request refused: HTTP {status} {}", .error.as_str())]
    Rejected { status: u16, error: OAuthErrorCode },
    /// The assertion could not be signed locally.
    #[error("OAuth assertion signing failed")]
    Signing,
}

/// Source of FCM access tokens for the worker. Object-safe so the Push
/// Service store holds an `Arc<dyn FcmAccessTokenSource>`.
pub trait FcmAccessTokenSource: Send + Sync + 'static {
    /// The cached token, refreshing it (single-flight) when it is
    /// missing or within [`FCM_ACCESS_TOKEN_REFRESH_MARGIN`] of expiry.
    fn current(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<FcmAccessToken, FcmTokenError>> + Send + '_>>;

    /// Drop the cached token if it is still `rejected`. Keyed on the
    /// rejected token so a burst of concurrent `401`s for one token
    /// causes exactly one refresh, and a late `401` for an already
    /// replaced token keeps the fresh one.
    fn invalidate(&self, rejected: &FcmAccessToken);
}

struct CachedAccessToken {
    token: FcmAccessToken,
    refresh_at: u64,
}

/// Why the token source's HTTP client could not be built.
#[derive(Debug, Error)]
pub enum FcmTokenSourceBuildError {
    #[error("failed to build the OAuth HTTP client: {0}")]
    Client(#[source] reqwest::Error),
    #[error("OAuth token URL is invalid")]
    TokenUrl,
}

/// JWT-bearer token source backed by Google's token endpoint.
pub struct OAuthFcmTokenSource {
    key: FcmServiceAccountKey,
    client: reqwest::Client,
    /// Test-only override of the token endpoint (a local mock server).
    /// Release builds have no such field: every exchange addresses
    /// [`GOOGLE_OAUTH_TOKEN_URL`] directly.
    #[cfg(test)]
    token_url: Url,
    clock: Arc<dyn FcmClock>,
    cache: RwLock<Option<CachedAccessToken>>,
    /// Held across the token exchange: concurrent refreshes queue here
    /// and then find the cache already filled.
    refresh: tokio::sync::Mutex<()>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

impl OAuthFcmTokenSource {
    pub fn try_new(
        key: FcmServiceAccountKey,
        clock: Arc<dyn FcmClock>,
    ) -> Result<Self, FcmTokenSourceBuildError> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .connect_timeout(Duration::from_secs(5))
            .timeout(TOKEN_REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(FcmTokenSourceBuildError::Client)?;
        // The endpoint is a constant; parsing it here proves at boot that
        // every exchange can address it.
        Url::parse(GOOGLE_OAUTH_TOKEN_URL).map_err(|_| FcmTokenSourceBuildError::TokenUrl)?;
        Ok(Self {
            key,
            client,
            #[cfg(test)]
            token_url: Url::parse(GOOGLE_OAUTH_TOKEN_URL)
                .map_err(|_| FcmTokenSourceBuildError::TokenUrl)?,
            clock,
            cache: RwLock::new(None),
            refresh: tokio::sync::Mutex::new(()),
        })
    }

    /// Test-only source pointed at a local mock server (cleartext).
    /// Never compiled into a release binary.
    #[cfg(test)]
    fn with_token_url(key: FcmServiceAccountKey, clock: Arc<dyn FcmClock>, token_url: Url) -> Self {
        let client = reqwest::Client::builder()
            .timeout(TOKEN_REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_default();
        Self {
            key,
            client,
            token_url,
            clock,
            cache: RwLock::new(None),
            refresh: tokio::sync::Mutex::new(()),
        }
    }

    /// The token endpoint: always the pinned Google URL in release
    /// builds, never a value read from configuration or the key file.
    #[cfg(not(test))]
    fn token_endpoint(&self) -> &'static str {
        GOOGLE_OAUTH_TOKEN_URL
    }

    #[cfg(test)]
    fn token_endpoint(&self) -> &str {
        self.token_url.as_str()
    }

    /// Serialize cache access without poisoning: mutations replace complete records,
    /// with no user callbacks or custom panicking destructors under this guard.
    fn lock_cache(&self) -> RwLockWriteGuard<'_, Option<CachedAccessToken>> {
        self.cache.write()
    }

    fn fresh_cached(&self) -> Option<FcmAccessToken> {
        let now = self.clock.now_unix_seconds();
        self.lock_cache()
            .as_ref()
            .filter(|cached| now < cached.refresh_at)
            .map(|cached| cached.token.clone())
    }

    async fn current_token(&self) -> Result<FcmAccessToken, FcmTokenError> {
        if let Some(token) = self.fresh_cached() {
            return Ok(token);
        }
        let _refresh = self.refresh.lock().await;
        // Another caller may have refreshed while this one waited.
        if let Some(token) = self.fresh_cached() {
            return Ok(token);
        }
        let issued_at = self.clock.now_unix_seconds();
        let (token, expires_in) =
            tokio::time::timeout(TOKEN_FETCH_TIMEOUT, self.exchange(issued_at))
                .await
                .map_err(|_| FcmTokenError::Transient(TransientFailure::Timeout))??;
        let refresh_at = issued_at
            .saturating_add(expires_in)
            .saturating_sub(FCM_ACCESS_TOKEN_REFRESH_MARGIN.as_secs());
        *self.lock_cache() = Some(CachedAccessToken {
            token: token.clone(),
            refresh_at,
        });
        Ok(token)
    }

    async fn exchange(&self, issued_at: u64) -> Result<(FcmAccessToken, u64), FcmTokenError> {
        let assertion = self.key.sign_assertion(issued_at).map_err(|_| {
            warn!("FCM OAuth assertion signing failed");
            FcmTokenError::Signing
        })?;
        let body = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", JWT_BEARER_GRANT_TYPE)
            .append_pair("assertion", &assertion)
            .finish();
        let response = self
            .client
            .post(self.token_endpoint())
            .header(
                CONTENT_TYPE,
                HeaderValue::from_static("application/x-www-form-urlencoded"),
            )
            .body(body)
            .send()
            .await
            .map_err(|error| {
                let failure = TransientFailure::from_transport_error(&error);
                warn!(error = %error.without_url(), "FCM OAuth token request failed");
                FcmTokenError::Transient(failure)
            })?;
        let status = response.status().as_u16();
        let body = read_body_bounded(response).await;
        match status {
            200 => {
                let parsed: TokenResponse =
                    serde_json::from_slice(&body).map_err(|_| FcmTokenError::MalformedResponse)?;
                if parsed.access_token.is_empty() {
                    return Err(FcmTokenError::MalformedResponse);
                }
                Ok((FcmAccessToken(parsed.access_token), parsed.expires_in))
            }
            429 | 500..=599 => Err(FcmTokenError::Transient(TransientFailure::ServerError {
                status,
            })),
            _ => Err(FcmTokenError::Rejected {
                status,
                error: OAuthErrorCode::from_body(&body),
            }),
        }
    }
}

impl std::fmt::Debug for OAuthFcmTokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthFcmTokenSource")
            .field("key", &self.key)
            .field("token_url", &self.token_endpoint())
            .finish_non_exhaustive()
    }
}

impl FcmAccessTokenSource for OAuthFcmTokenSource {
    fn current(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<FcmAccessToken, FcmTokenError>> + Send + '_>> {
        Box::pin(self.current_token())
    }

    fn invalidate(&self, rejected: &FcmAccessToken) {
        let mut cache = self.lock_cache();
        if cache
            .as_ref()
            .is_some_and(|cached| &cached.token == rejected)
        {
            *cache = None;
        }
    }
}

/// Read at most [`MAX_TOKEN_RESPONSE_BYTES`]; a longer body is not
/// Google's shape and reads as empty.
async fn read_body_bounded(mut response: reqwest::Response) -> Vec<u8> {
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        if body.len() + chunk.len() > MAX_TOKEN_RESPONSE_BYTES {
            return Vec::new();
        }
        body.extend_from_slice(&chunk);
    }
    body
}

#[cfg(test)]
pub(crate) mod test_keys {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;

    fn pem(label: &str, der: &[u8]) -> String {
        let encoded = STANDARD.encode(der);
        let mut out = format!("-----BEGIN {label}-----\n");
        for line in encoded.as_bytes().chunks(64) {
            out.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
            out.push('\n');
        }
        out.push_str(&format!("-----END {label}-----\n"));
        out
    }

    /// A fresh RSA-2048 key as (PKCS#8 private PEM, SPKI public PEM),
    /// the formats Google issues service-account keys in.
    pub(crate) fn rsa_key_pair() -> (String, String) {
        use aws_lc_rs::encoding::{AsDer, Pkcs8V1Der, PublicKeyX509Der};
        use aws_lc_rs::signature::KeyPair as _;
        let pair = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048)
            .expect("generate RSA key");
        let private: Pkcs8V1Der<'static> = pair.as_der().expect("PKCS#8 DER");
        let public: PublicKeyX509Der<'static> = pair.public_key().as_der().expect("SPKI DER");
        (
            pem("PRIVATE KEY", private.as_ref()),
            pem("PUBLIC KEY", public.as_ref()),
        )
    }

    /// A service-account key file around `private_pem`.
    pub(crate) fn service_account_json(private_pem: &str, token_uri: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "type": "service_account",
            "project_id": "waddle-test",
            "private_key_id": "key-id-1",
            "private_key": private_pem,
            "client_email": "fcm-sender@waddle-test.iam.gserviceaccount.com",
            "client_id": "1234567890",
            "token_uri": token_uri,
        }))
        .expect("key JSON")
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    use jsonwebtoken::{DecodingKey, Validation};

    use super::test_keys::{rsa_key_pair, service_account_json};
    use super::*;

    const START: u64 = 1_700_000_000;

    struct ManualClock(AtomicU64);

    impl ManualClock {
        fn advance(&self, by: Duration) {
            self.0.fetch_add(by.as_secs(), Ordering::SeqCst);
        }
    }

    impl FcmClock for ManualClock {
        fn now_unix_seconds(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct Fixture {
        server: mockito::ServerGuard,
        source: Arc<OAuthFcmTokenSource>,
        clock: Arc<ManualClock>,
        public_pem: String,
    }

    async fn fixture() -> Fixture {
        let server = mockito::Server::new_async().await;
        let (private_pem, public_pem) = rsa_key_pair();
        // The key file names the mock server; the source must ignore it.
        let key = FcmServiceAccountKey::from_json(&service_account_json(
            &private_pem,
            "https://evil.example/token",
        ))
        .expect("service-account key");
        let clock = Arc::new(ManualClock(AtomicU64::new(START)));
        let token_url = Url::parse(&format!("{}/token", server.url())).expect("mock url");
        let source = Arc::new(OAuthFcmTokenSource::with_token_url(
            key,
            Arc::clone(&clock) as Arc<dyn FcmClock>,
            token_url,
        ));
        Fixture {
            server,
            source,
            clock,
            public_pem,
        }
    }

    fn token_body(token: &str, expires_in: u64) -> String {
        serde_json::json!({
            "access_token": token,
            "expires_in": expires_in,
            "token_type": "Bearer",
        })
        .to_string()
    }

    #[tokio::test]
    async fn production_client_connect_timeout_preserves_pre_send_evidence() {
        let (private_pem, _) = rsa_key_pair();
        let key = FcmServiceAccountKey::from_json(&service_account_json(&private_pem, ""))
            .expect("service-account key");
        let source = OAuthFcmTokenSource::try_new(key, Arc::new(SystemFcmClock))
            .expect("production token source");
        crate::push::types::assert_production_connect_timeout(source.client).await;
    }

    #[derive(Deserialize)]
    struct DecodedClaims {
        iss: String,
        scope: String,
        aud: String,
        iat: u64,
        exp: u64,
    }

    #[test]
    fn rejects_malformed_key_files() {
        assert!(matches!(
            FcmServiceAccountKey::from_json(b"not json"),
            Err(FcmServiceAccountError::NotJson)
        ));
        assert!(matches!(
            FcmServiceAccountKey::from_json(br#"{"type":"authorized_user"}"#),
            Err(FcmServiceAccountError::WrongType(_))
        ));
        assert!(matches!(
            FcmServiceAccountKey::from_json(br#"{"type":"service_account","private_key":"x"}"#),
            Err(FcmServiceAccountError::MissingField("client_email"))
        ));
        let garbage = serde_json::to_vec(&serde_json::json!({
            "type": "service_account",
            "client_email": "a@b.iam.gserviceaccount.com",
            "private_key": "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
        }))
        .expect("json");
        assert!(matches!(
            FcmServiceAccountKey::from_json(&garbage),
            Err(FcmServiceAccountError::PrivateKey(_))
        ));
    }

    #[test]
    fn key_and_token_debug_are_redacted() {
        let (private_pem, _) = rsa_key_pair();
        let key =
            FcmServiceAccountKey::from_json(&service_account_json(&private_pem, "")).expect("key");
        let rendered = format!("{key:?}");
        assert!(rendered.contains("fcm-sender@waddle-test.iam.gserviceaccount.com"));
        assert!(!rendered.contains("PRIVATE KEY"));
        let token = FcmAccessToken::new("ya29.secret".to_string());
        assert!(!format!("{token:?}").contains("ya29"));
    }

    #[tokio::test]
    async fn exchanges_a_jwt_bearer_assertion_at_the_token_endpoint() {
        let mut fixture = fixture().await;
        let captured: Arc<Mutex<Option<String>>> = Arc::default();
        let capture = Arc::clone(&captured);
        let mock = fixture
            .server
            .mock("POST", "/token")
            .match_header("content-type", "application/x-www-form-urlencoded")
            .match_body(mockito::Matcher::UrlEncoded(
                "grant_type".into(),
                JWT_BEARER_GRANT_TYPE.into(),
            ))
            .match_request(move |request| {
                let assertion = request.body().ok().and_then(|body| {
                    url::form_urlencoded::parse(body)
                        .find(|(name, _)| name == "assertion")
                        .map(|(_, value)| value.into_owned())
                });
                *capture.lock().expect("capture lock") = assertion;
                true
            })
            .with_status(200)
            .with_body(token_body("ya29.first", 3600))
            .create_async()
            .await;
        let token = fixture.source.current().await.expect("token");
        mock.assert_async().await;
        assert_eq!(token.as_str(), "ya29.first");

        let assertion = captured
            .lock()
            .expect("capture lock")
            .clone()
            .expect("assertion form field");
        let public_pem = fixture.public_pem.clone();
        let header = jsonwebtoken::decode_header(&assertion).expect("header");
        assert_eq!(header.alg, Algorithm::RS256);
        assert_eq!(header.kid.as_deref(), Some("key-id-1"));
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[GOOGLE_OAUTH_TOKEN_URL]);
        validation.validate_exp = false;
        let claims = jsonwebtoken::decode::<DecodedClaims>(
            &assertion,
            &DecodingKey::from_rsa_pem(public_pem.as_bytes()).expect("public key"),
            &validation,
        )
        .expect("valid RS256 signature")
        .claims;
        assert_eq!(claims.iss, "fcm-sender@waddle-test.iam.gserviceaccount.com");
        assert_eq!(claims.scope, FCM_OAUTH_SCOPE);
        assert_eq!(claims.aud, GOOGLE_OAUTH_TOKEN_URL);
        assert_eq!(claims.iat, START);
        assert_eq!(claims.exp, START + 3600);
    }

    #[test]
    fn production_source_pins_the_google_token_endpoint() {
        let (private_pem, _) = rsa_key_pair();
        let key = FcmServiceAccountKey::from_json(&service_account_json(
            &private_pem,
            "https://evil.example/token",
        ))
        .expect("key");
        let source = OAuthFcmTokenSource::try_new(key, Arc::new(SystemFcmClock)).expect("source");
        assert_eq!(source.token_url.as_str(), GOOGLE_OAUTH_TOKEN_URL);
    }

    #[tokio::test]
    async fn token_is_cached_until_five_minutes_before_expiry() {
        let mut fixture = fixture().await;
        let first = fixture
            .server
            .mock("POST", "/token")
            .with_status(200)
            .with_body(token_body("ya29.first", 3600))
            .expect(1)
            .create_async()
            .await;
        let token = fixture.source.current().await.expect("first");
        fixture.clock.advance(
            Duration::from_secs(3600) - FCM_ACCESS_TOKEN_REFRESH_MARGIN - Duration::from_secs(1),
        );
        assert_eq!(fixture.source.current().await.expect("cached"), token);
        first.assert_async().await;
        first.remove_async().await;

        let second = fixture
            .server
            .mock("POST", "/token")
            .with_status(200)
            .with_body(token_body("ya29.second", 3600))
            .expect(1)
            .create_async()
            .await;
        fixture.clock.advance(Duration::from_secs(1));
        assert_eq!(
            fixture.source.current().await.expect("refreshed").as_str(),
            "ya29.second"
        );
        second.assert_async().await;
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_refresh() {
        let mut fixture = fixture().await;
        let mock = fixture
            .server
            .mock("POST", "/token")
            .with_status(200)
            .with_body(token_body("ya29.shared", 3600))
            .expect(1)
            .create_async()
            .await;
        let callers: Vec<_> = (0..8)
            .map(|_| {
                let source = Arc::clone(&fixture.source);
                tokio::spawn(async move { source.current().await })
            })
            .collect();
        for caller in callers {
            assert_eq!(
                caller.await.expect("join").expect("token").as_str(),
                "ya29.shared"
            );
        }
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn cache_unwind_preserves_reuse_and_single_flight_refresh() {
        let mut fixture = fixture().await;
        let first = fixture
            .server
            .mock("POST", "/token")
            .with_status(200)
            .with_body(token_body("ya29.first", 3600))
            .expect(1)
            .create_async()
            .await;
        let stale = fixture.source.current().await.expect("first");
        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _cache = fixture.source.lock_cache();
            panic!("fault while holding the populated cache guard");
        }));
        assert!(panic.is_err());
        assert_eq!(
            fixture.source.current().await.expect("cached after unwind"),
            stale
        );
        first.assert_async().await;
        first.remove_async().await;

        let second = fixture
            .server
            .mock("POST", "/token")
            .with_status(200)
            .with_body(token_body("ya29.second", 3600))
            .expect(1)
            .create_async()
            .await;
        fixture.source.invalidate(&stale);
        let callers: Vec<_> = (0..8)
            .map(|_| {
                let source = Arc::clone(&fixture.source);
                tokio::spawn(async move { source.current().await })
            })
            .collect();
        for caller in callers {
            assert_eq!(
                caller
                    .await
                    .expect("join")
                    .expect("refreshed token")
                    .as_str(),
                "ya29.second"
            );
        }
        fixture.source.invalidate(&stale);
        assert_eq!(
            fixture
                .source
                .current()
                .await
                .expect("newer token retained")
                .as_str(),
            "ya29.second"
        );
        second.assert_async().await;
    }

    #[tokio::test]
    async fn invalidate_is_keyed_on_the_rejected_token() {
        let mut fixture = fixture().await;
        let first = fixture
            .server
            .mock("POST", "/token")
            .with_status(200)
            .with_body(token_body("ya29.first", 3600))
            .expect(1)
            .create_async()
            .await;
        let stale = fixture.source.current().await.expect("first");
        first.assert_async().await;
        first.remove_async().await;
        let second = fixture
            .server
            .mock("POST", "/token")
            .with_status(200)
            .with_body(token_body("ya29.second", 3600))
            .expect(1)
            .create_async()
            .await;
        fixture.source.invalidate(&stale);
        let fresh = fixture.source.current().await.expect("fresh");
        assert_eq!(fresh.as_str(), "ya29.second");
        // A late 401 for the already-replaced token keeps the fresh one.
        fixture.source.invalidate(&stale);
        assert_eq!(fixture.source.current().await.expect("still fresh"), fresh);
        second.assert_async().await;
    }

    #[tokio::test]
    async fn invalid_grant_is_a_permanent_rejection() {
        let mut fixture = fixture().await;
        let mock = fixture
            .server
            .mock("POST", "/token")
            .with_status(400)
            .with_body(r#"{"error":"invalid_grant","error_description":"Invalid JWT Signature."}"#)
            .create_async()
            .await;
        assert_eq!(
            fixture.source.current().await,
            Err(FcmTokenError::Rejected {
                status: 400,
                error: OAuthErrorCode::InvalidGrant,
            })
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn token_endpoint_outages_are_transient() {
        let mut fixture = fixture().await;
        let mock = fixture
            .server
            .mock("POST", "/token")
            .with_status(503)
            .create_async()
            .await;
        assert_eq!(
            fixture.source.current().await,
            Err(FcmTokenError::Transient(TransientFailure::ServerError {
                status: 503
            }))
        );
        mock.assert_async().await;

        let (private_pem, _) = rsa_key_pair();
        let unreachable = OAuthFcmTokenSource::with_token_url(
            FcmServiceAccountKey::from_json(&service_account_json(&private_pem, "")).expect("key"),
            Arc::new(SystemFcmClock),
            Url::parse("http://127.0.0.1:1/token").expect("url"),
        );
        assert_eq!(
            unreachable.current().await,
            Err(FcmTokenError::Transient(TransientFailure::Connect))
        );
    }

    #[tokio::test]
    async fn a_200_without_a_token_is_malformed() {
        let mut fixture = fixture().await;
        let mock = fixture
            .server
            .mock("POST", "/token")
            .with_status(200)
            .with_body(r#"{"token_type":"Bearer"}"#)
            .create_async()
            .await;
        assert_eq!(
            fixture.source.current().await,
            Err(FcmTokenError::MalformedResponse)
        );
        mock.assert_async().await;
    }
}
