//! RFC 8628 device-flow sign-in against a Waddle server.
//!
//! The same flow the Apple and Android apps use: start a device grant for a
//! provider, show the user code, open the verification page in the browser,
//! and poll until the server reports the grant complete. The result is a
//! session id, which the XMPP stream later presents as its OAUTHBEARER token.
//!
//! This runs before any XMPP stream exists, which is why it is plain HTTP.
//! ADR-0019 moves it into `waddle-client-session-tokio` once that crate
//! exists; the wire shapes here are the server's (`routes/device.rs`).

use std::fmt;
use std::time::Duration;

use jid::BareJid;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

/// The server a user signs in to: an absolute https URL, or http for a
/// loopback development server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerUrl(Url);

impl ServerUrl {
    /// Parse what the user typed. A bare host (`xmpp.waddle.social`) means
    /// https. Paths, queries and fragments are dropped: the API lives at the
    /// origin.
    pub fn parse(input: &str) -> Result<Self, DeviceAuthError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(DeviceAuthError::InvalidServerUrl);
        }
        let candidate = if trimmed.contains("://") {
            trimmed.to_string()
        } else {
            format!("https://{trimmed}")
        };
        let url = Url::parse(&candidate).map_err(|_| DeviceAuthError::InvalidServerUrl)?;
        let loopback = match url.host() {
            Some(url::Host::Domain(host)) => host == "localhost",
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        match url.scheme() {
            "https" => {}
            "http" if loopback => {}
            _ => return Err(DeviceAuthError::InsecureServerUrl),
        }
        if url.host().is_none() || !url.username().is_empty() || url.password().is_some() {
            return Err(DeviceAuthError::InvalidServerUrl);
        }
        let origin = url.origin().ascii_serialization();
        Url::parse(&origin)
            .map(Self)
            .map_err(|_| DeviceAuthError::InvalidServerUrl)
    }

    pub fn as_url(&self) -> &Url {
        &self.0
    }

    fn endpoint(&self, path: &str) -> Url {
        // `path` is always one of this module's absolute API paths, so the
        // join cannot leave the origin.
        let mut url = self.0.clone();
        url.set_path(path);
        url
    }
}

impl fmt::Display for ServerUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str().trim_end_matches('/'))
    }
}

/// A sign-in provider the server offers (GitHub, an OIDC issuer, …).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Provider {
    pub id: String,
    pub display_name: String,
}

/// A started device grant: what to show the user and how to poll for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthorization {
    device_code: DeviceCode,
    pub user_code: String,
    /// The page the user approves the code on. Always http(s).
    pub verification_url: Url,
    pub interval: Duration,
    pub expires_in: Duration,
}

impl DeviceAuthorization {
    pub fn device_code(&self) -> &DeviceCode {
        &self.device_code
    }
}

/// The device code: a credential for the duration of the grant, so it never
/// prints.
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct DeviceCode(String);

impl fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceCode(…)")
    }
}

/// A Waddle session id: the bearer token for the XMPP stream. Never prints.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionId(…)")
    }
}

/// A completed sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedInSession {
    pub session_id: SessionId,
    pub username: String,
    pub jid: BareJid,
    pub websocket_url: Url,
}

/// One poll's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    Pending,
    Complete(SignedInSession),
}

#[derive(Debug, Error)]
pub enum DeviceAuthError {
    #[error("enter the address of a Waddle server")]
    InvalidServerUrl,
    #[error("the server address must use https")]
    InsecureServerUrl,
    #[error("could not reach the server")]
    Http(#[from] reqwest::Error),
    #[error("the sign-in code expired; start again")]
    Expired,
    #[error("the server refused the sign-in: {message}")]
    Refused { error: String, message: String },
    #[error("the server sent an answer this app does not understand")]
    UnexpectedResponse,
}

#[derive(Deserialize)]
struct StartResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    interval: u32,
    expires_in: u32,
}

#[derive(Deserialize)]
struct PollResponse {
    status: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    jid: Option<String>,
    #[serde(default)]
    websocket_url: Option<String>,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: String,
    message: String,
}

/// Talks to one server's device-flow endpoints.
#[derive(Debug, Clone)]
pub struct DeviceAuthClient {
    http: reqwest::Client,
    server: ServerUrl,
}

impl DeviceAuthClient {
    pub fn new(http: reqwest::Client, server: ServerUrl) -> Self {
        Self { http, server }
    }

    pub fn server(&self) -> &ServerUrl {
        &self.server
    }

    pub async fn providers(&self) -> Result<Vec<Provider>, DeviceAuthError> {
        let response = self
            .http
            .get(self.server.endpoint("/api/auth/providers"))
            .send()
            .await?;
        decode(response).await
    }

    pub async fn start(&self, provider: &Provider) -> Result<DeviceAuthorization, DeviceAuthError> {
        #[derive(Serialize)]
        struct Request<'a> {
            provider: &'a str,
        }
        let response = self
            .http
            .post(self.server.endpoint("/api/auth/device/start"))
            .json(&Request {
                provider: &provider.id,
            })
            .send()
            .await?;
        let body: StartResponse = decode(response).await?;
        self.authorization_from(body)
    }

    pub async fn poll(
        &self,
        authorization: &DeviceAuthorization,
    ) -> Result<PollOutcome, DeviceAuthError> {
        #[derive(Serialize)]
        struct Request<'a> {
            device_code: &'a DeviceCode,
        }
        let response = self
            .http
            .post(self.server.endpoint("/api/auth/device/poll"))
            .json(&Request {
                device_code: &authorization.device_code,
            })
            .send()
            .await?;
        let body: PollResponse = decode(response).await?;
        poll_outcome(body)
    }

    /// Poll until the grant completes, waiting the server's interval between
    /// polls, and give up once the grant's lifetime has passed.
    pub async fn wait_for_approval(
        &self,
        authorization: &DeviceAuthorization,
    ) -> Result<SignedInSession, DeviceAuthError> {
        let deadline = tokio::time::Instant::now() + authorization.expires_in;
        loop {
            tokio::time::sleep(authorization.interval).await;
            if tokio::time::Instant::now() >= deadline {
                return Err(DeviceAuthError::Expired);
            }
            if let PollOutcome::Complete(session) = self.poll(authorization).await? {
                return Ok(session);
            }
        }
    }

    fn authorization_from(
        &self,
        body: StartResponse,
    ) -> Result<DeviceAuthorization, DeviceAuthError> {
        // Only ever open an http(s) page, whatever the server answered; fall
        // back to the plain verification page, then to the known route.
        let verification_url = body
            .verification_uri_complete
            .as_deref()
            .and_then(web_page)
            .or_else(|| web_page(&body.verification_uri))
            .unwrap_or_else(|| {
                let mut url = self.server.endpoint("/api/auth/device/verify");
                url.query_pairs_mut().append_pair("code", &body.user_code);
                url
            });
        Ok(DeviceAuthorization {
            device_code: DeviceCode(body.device_code),
            user_code: body.user_code,
            verification_url,
            // RFC 8628 §3.2: five seconds when the server gives no interval.
            interval: Duration::from_secs(u64::from(body.interval.max(1))),
            expires_in: Duration::from_secs(u64::from(body.expires_in)),
        })
    }
}

fn web_page(raw: &str) -> Option<Url> {
    Url::parse(raw)
        .ok()
        .filter(|url| matches!(url.scheme(), "https" | "http") && url.host().is_some())
}

fn poll_outcome(body: PollResponse) -> Result<PollOutcome, DeviceAuthError> {
    match body.status.as_str() {
        "authorization_pending" => Ok(PollOutcome::Pending),
        "complete" => {
            let (Some(session_id), Some(username), Some(jid), Some(websocket_url)) =
                (body.session_id, body.username, body.jid, body.websocket_url)
            else {
                return Err(DeviceAuthError::UnexpectedResponse);
            };
            let jid = jid
                .parse::<BareJid>()
                .map_err(|_| DeviceAuthError::UnexpectedResponse)?;
            let websocket_url = Url::parse(&websocket_url)
                .ok()
                .filter(|url| matches!(url.scheme(), "wss" | "ws"))
                .ok_or(DeviceAuthError::UnexpectedResponse)?;
            Ok(PollOutcome::Complete(SignedInSession {
                session_id: SessionId(session_id),
                username,
                jid,
                websocket_url,
            }))
        }
        _ => Err(DeviceAuthError::UnexpectedResponse),
    }
}

async fn decode<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, DeviceAuthError> {
    let status = response.status();
    let bytes = response.bytes().await?;
    if status.is_success() {
        return serde_json::from_slice(&bytes).map_err(|_| DeviceAuthError::UnexpectedResponse);
    }
    match serde_json::from_slice::<ErrorBody>(&bytes) {
        Ok(body) if body.error == "expired_token" || body.error == "invalid_device_code" => {
            Err(DeviceAuthError::Expired)
        }
        Ok(body) => Err(DeviceAuthError::Refused {
            error: body.error,
            message: body.message,
        }),
        Err(_) => Err(DeviceAuthError::UnexpectedResponse),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_url_defaults_to_https_and_keeps_only_the_origin() {
        let url = ServerUrl::parse(" xmpp.waddle.social/some/path?x=1 ").unwrap();
        assert_eq!(url.to_string(), "https://xmpp.waddle.social");
        assert_eq!(
            url.endpoint("/api/auth/providers").as_str(),
            "https://xmpp.waddle.social/api/auth/providers"
        );
    }

    #[test]
    fn server_url_allows_http_only_on_loopback() {
        assert!(ServerUrl::parse("http://localhost:3000").is_ok());
        assert!(ServerUrl::parse("http://127.0.0.1:3000").is_ok());
        assert!(matches!(
            ServerUrl::parse("http://xmpp.waddle.social"),
            Err(DeviceAuthError::InsecureServerUrl)
        ));
        assert!(matches!(
            ServerUrl::parse("ftp://xmpp.waddle.social"),
            Err(DeviceAuthError::InsecureServerUrl)
        ));
        assert!(matches!(
            ServerUrl::parse("  "),
            Err(DeviceAuthError::InvalidServerUrl)
        ));
        assert!(matches!(
            ServerUrl::parse("https://user:pw@xmpp.waddle.social"),
            Err(DeviceAuthError::InvalidServerUrl)
        ));
    }

    fn client() -> DeviceAuthClient {
        crate::install_crypto_provider();
        DeviceAuthClient::new(
            reqwest::Client::new(),
            ServerUrl::parse("https://xmpp.waddle.social").unwrap(),
        )
    }

    fn start(complete: Option<&str>, plain: &str) -> StartResponse {
        StartResponse {
            device_code: "device-secret".into(),
            user_code: "ABCD-1234".into(),
            verification_uri: plain.into(),
            verification_uri_complete: complete.map(Into::into),
            interval: 5,
            expires_in: 600,
        }
    }

    #[test]
    fn verification_page_is_always_http() {
        let client = client();
        let auth = client
            .authorization_from(start(
                Some("https://xmpp.waddle.social/api/auth/device/verify?code=ABCD-1234"),
                "https://xmpp.waddle.social/api/auth/device/verify",
            ))
            .unwrap();
        assert_eq!(
            auth.verification_url.as_str(),
            "https://xmpp.waddle.social/api/auth/device/verify?code=ABCD-1234"
        );

        let auth = client
            .authorization_from(start(Some("javascript:alert(1)"), "file:///etc/passwd"))
            .unwrap();
        assert_eq!(
            auth.verification_url.as_str(),
            "https://xmpp.waddle.social/api/auth/device/verify?code=ABCD-1234"
        );
        assert_eq!(auth.interval, Duration::from_secs(5));
        assert_eq!(auth.expires_in, Duration::from_secs(600));
    }

    #[test]
    fn secrets_never_print() {
        let auth = client()
            .authorization_from(start(None, "https://xmpp.waddle.social/v"))
            .unwrap();
        assert!(!format!("{auth:?}").contains("device-secret"));
        assert!(!format!("{:?}", SessionId("s3cret".into())).contains("s3cret"));
    }

    fn poll(json: &str) -> Result<PollOutcome, DeviceAuthError> {
        poll_outcome(serde_json::from_str(json).unwrap())
    }

    #[test]
    fn poll_pending_and_complete() {
        assert_eq!(
            poll(r#"{"status":"authorization_pending","expires_in":540}"#).unwrap(),
            PollOutcome::Pending
        );
        let PollOutcome::Complete(session) = poll(
            r#"{"status":"complete","session_id":"sess","username":"david","provider_id":"github",
                "jid":"david@waddle.social","xmpp_host":"waddle.social",
                "websocket_url":"wss://xmpp.waddle.social/ws"}"#,
        )
        .unwrap() else {
            panic!("expected a completed session");
        };
        assert_eq!(session.jid.to_string(), "david@waddle.social");
        assert_eq!(session.session_id.expose(), "sess");
        assert_eq!(
            session.websocket_url.as_str(),
            "wss://xmpp.waddle.social/ws"
        );
    }

    #[test]
    fn poll_rejects_incomplete_or_unsafe_answers() {
        for json in [
            r#"{"status":"complete","session_id":"s","username":"u","jid":"u@x"}"#,
            r#"{"status":"complete","session_id":"s","username":"u","jid":"not a jid@@","websocket_url":"wss://x/ws"}"#,
            r#"{"status":"complete","session_id":"s","username":"u","jid":"u@x","websocket_url":"https://x/ws"}"#,
            r#"{"status":"slow_down"}"#,
        ] {
            assert!(
                matches!(poll(json), Err(DeviceAuthError::UnexpectedResponse)),
                "{json}"
            );
        }
    }

    /// The whole flow against a local stand-in for the server's routes.
    #[tokio::test]
    async fn signs_in_through_the_device_flow() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut polls = 0;
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = vec![0; 4096];
                let read = socket.read(&mut buffer).await.unwrap();
                let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                let body = if request.starts_with("GET /api/auth/providers") {
                    r#"[{"id":"github","display_name":"GitHub","kind":"oauth2"}]"#.to_string()
                } else if request.starts_with("POST /api/auth/device/start") {
                    assert!(request.contains(r#""provider":"github""#));
                    r#"{"device_code":"dc","user_code":"WXYZ-0001","verification_uri":"http://127.0.0.1/v",
                        "verification_uri_complete":"http://127.0.0.1/v?code=WXYZ-0001","interval":1,"expires_in":30}"#
                        .to_string()
                } else if request.starts_with("POST /api/auth/device/poll") {
                    assert!(request.contains(r#""device_code":"dc""#));
                    polls += 1;
                    if polls == 1 {
                        r#"{"status":"authorization_pending","expires_in":29}"#.to_string()
                    } else {
                        r#"{"status":"complete","session_id":"sess","username":"david","provider_id":"github",
                            "jid":"david@localhost","xmpp_host":"localhost","websocket_url":"ws://127.0.0.1/ws"}"#
                            .to_string()
                    }
                } else {
                    panic!("unexpected request: {request}");
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
                if polls == 2 {
                    break;
                }
            }
        });

        crate::install_crypto_provider();
        let client = DeviceAuthClient::new(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            ServerUrl::parse(&format!("http://{address}")).unwrap(),
        );
        let providers = client.providers().await.unwrap();
        assert_eq!(providers[0].display_name, "GitHub");
        let authorization = client.start(&providers[0]).await.unwrap();
        assert_eq!(authorization.user_code, "WXYZ-0001");
        let session = client.wait_for_approval(&authorization).await.unwrap();
        assert_eq!(session.jid.to_string(), "david@localhost");
        server.await.unwrap();
    }
}
