//! Browser-session authentication shared by JSON API routes.
//!
//! A request presents its session id either in the `x-waddle-session-id`
//! header (the chat app's fragment-delivered session) or in the
//! `waddle_session` cookie. The header wins when both are present.
//!
//! Cookie-authenticated mutations are CSRF-checked (D20): `SameSite=Lax`
//! still sends the cookie from same-site sibling origins, so a request that
//! authenticates by cookie and uses a method other than GET, HEAD or OPTIONS
//! must carry an `Origin` that [`TrustedWebOrigins`] accepts. A header can
//! only be set by script running on an origin CORS already allows, so
//! header-authenticated requests are exempt.
//!
//! Error bodies never echo the presented session id.
//!
//! [`TrustedWebOrigins`]: crate::auth::redirect::TrustedWebOrigins

use std::sync::Arc;

use axum::{
    extract::{FromRef, FromRequestParts},
    http::{header, request::Parts, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use jid::BareJid;
use serde::Serialize;

use super::auth::AuthState;
use crate::auth::AuthError;

pub(crate) const SESSION_ID_HEADER: &str = "x-waddle-session-id";
const SESSION_COOKIE: &str = "waddle_session";
const NO_STORE: &str = "no-store";

/// The `waddle_session` cookie value, if present and non-empty.
pub(crate) fn extract_session_cookie(headers: &HeaderMap) -> Option<String> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in cookie_header.split(';') {
        let trimmed = pair.trim();
        let Some((name, value)) = trimmed.split_once('=') else {
            continue;
        };
        if name == SESSION_COOKIE && !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// The trimmed `x-waddle-session-id` header value, if present and non-empty.
pub(crate) fn extract_session_header(headers: &HeaderMap) -> Option<String> {
    let session_id = headers.get(SESSION_ID_HEADER)?.to_str().ok()?.trim();
    if session_id.is_empty() {
        return None;
    }
    Some(session_id.to_string())
}

/// How the request presented its session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionTransport {
    Header,
    Cookie,
}

/// The authenticated human user behind a request.
#[derive(Debug, Clone)]
pub(crate) struct SessionPrincipal {
    pub user_jid: BareJid,
    pub transport: SessionTransport,
}

/// A JSON API error: `{ "error": "<code>", "message": "<static text>" }`
/// with `Cache-Control: no-store`. Both fields are static, so no request
/// input can be reflected into a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

#[derive(Serialize)]
struct ApiErrorBody {
    error: &'static str,
    message: &'static str,
}

impl ApiError {
    pub(crate) const fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
        }
    }

    #[cfg(test)]
    pub(crate) fn status(&self) -> StatusCode {
        self.status
    }

    #[cfg(test)]
    pub(crate) fn code(&self) -> &'static str {
        self.code
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            [(header::CACHE_CONTROL, NO_STORE)],
            Json(ApiErrorBody {
                error: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

const MISSING_SESSION: ApiError = ApiError::new(
    StatusCode::UNAUTHORIZED,
    "invalid_session",
    "a valid session is required",
);
const SESSION_EXPIRED: ApiError = ApiError::new(
    StatusCode::UNAUTHORIZED,
    "session_expired",
    "the session has expired",
);
const SESSION_UNAVAILABLE: ApiError = ApiError::new(
    StatusCode::SERVICE_UNAVAILABLE,
    "session_unavailable",
    "sessions are temporarily unavailable",
);
const UNTRUSTED_ORIGIN: ApiError = ApiError::new(
    StatusCode::FORBIDDEN,
    "untrusted_origin",
    "cookie-authenticated changes must come from a trusted origin",
);

/// Map a session validation failure to its API error. The presented id is
/// never part of the response.
fn session_error(error: &AuthError) -> ApiError {
    match error {
        AuthError::SessionExpired => SESSION_EXPIRED,
        AuthError::DatabaseError(_) => SESSION_UNAVAILABLE,
        _ => MISSING_SESSION,
    }
}

/// D20: a cookie-authenticated, state-changing request needs a trusted
/// `Origin`.
fn csrf_guard(
    transport: SessionTransport,
    method: &Method,
    headers: &HeaderMap,
    auth: &AuthState,
) -> Result<(), ApiError> {
    let safe = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    if transport == SessionTransport::Header || safe {
        return Ok(());
    }
    let trusted = headers
        .get(header::ORIGIN)
        .and_then(|origin| origin.to_str().ok())
        .is_some_and(|origin| auth.trusted_web_origins.is_trusted_origin(origin));
    if trusted {
        Ok(())
    } else {
        Err(UNTRUSTED_ORIGIN)
    }
}

impl<S> FromRequestParts<S> for SessionPrincipal
where
    Arc<AuthState>: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let auth = Arc::<AuthState>::from_ref(state);
        let (session_id, transport) = match extract_session_header(&parts.headers) {
            Some(id) => (id, SessionTransport::Header),
            None => match extract_session_cookie(&parts.headers) {
                Some(id) => (id, SessionTransport::Cookie),
                None => return Err(MISSING_SESSION),
            },
        };
        csrf_guard(transport, &parts.method, &parts.headers, &auth)?;

        let session = auth
            .session_manager
            .validate_session(&session_id)
            .await
            .map_err(|error| session_error(&error))?;
        let user_jid = session
            .user_jid
            .parse::<BareJid>()
            .map_err(|_| MISSING_SESSION)?;
        Ok(Self {
            user_jid,
            transport,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Session;
    use crate::config::ServerConfig;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use crate::server::AppState;
    use axum::http::{HeaderValue, Request};

    async fn auth_state() -> Arc<AuthState> {
        let db_pool = DatabasePool::new(DatabaseConfig::default(), PoolConfig)
            .await
            .expect("db pool");
        MigrationRunner::global()
            .run(db_pool.global())
            .await
            .expect("migrations");
        Arc::new(AuthState::new(
            Arc::new(AppState::new(Arc::new(db_pool))),
            &ServerConfig::test_homeserver(),
            &url::Url::parse("wss://xmpp.example.com/ws").expect("test WebSocket URL"),
            Some(b"test-session-key"),
        ))
    }

    async fn session(auth: &AuthState) -> Session {
        let session = Session::new("alice@localhost", "alice", "alice");
        auth.session_manager
            .create_session(&session)
            .await
            .expect("create session");
        session
    }

    async fn extract(
        auth: &Arc<AuthState>,
        request: Request<()>,
    ) -> Result<SessionPrincipal, ApiError> {
        let (mut parts, ()) = request.into_parts();
        SessionPrincipal::from_request_parts(&mut parts, auth).await
    }

    #[test]
    fn session_auth_cookie_parser_is_strict() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("theme=dark; waddle_session=abc; other=1"),
        );
        assert_eq!(extract_session_cookie(&headers).as_deref(), Some("abc"));

        headers.insert(header::COOKIE, HeaderValue::from_static("waddle_session="));
        assert_eq!(extract_session_cookie(&headers), None);
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("not_waddle_session=abc"),
        );
        assert_eq!(extract_session_cookie(&headers), None);
    }

    #[test]
    fn session_auth_header_parser_trims_and_rejects_blank() {
        let mut headers = HeaderMap::new();
        headers.insert(SESSION_ID_HEADER, HeaderValue::from_static("  abc  "));
        assert_eq!(extract_session_header(&headers).as_deref(), Some("abc"));
        headers.insert(SESSION_ID_HEADER, HeaderValue::from_static("   "));
        assert_eq!(extract_session_header(&headers), None);
    }

    #[test]
    fn session_auth_error_mapping() {
        assert_eq!(
            session_error(&AuthError::SessionNotFound("id".into())),
            MISSING_SESSION
        );
        assert_eq!(session_error(&AuthError::SessionExpired), SESSION_EXPIRED);
        assert_eq!(
            session_error(&AuthError::DatabaseError("down".into())).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn session_auth_prefers_header_over_cookie() {
        let auth = auth_state().await;
        let session = session(&auth).await;
        let principal = extract(
            &auth,
            Request::post("/")
                .header(SESSION_ID_HEADER, &session.id)
                .header(header::COOKIE, "waddle_session=other")
                .body(())
                .expect("request"),
        )
        .await
        .expect("principal");
        assert_eq!(principal.transport, SessionTransport::Header);
        assert_eq!(principal.user_jid.as_str(), "alice@localhost");
    }

    #[tokio::test]
    async fn session_auth_missing_or_unknown_session_is_invalid_session() {
        let auth = auth_state().await;
        let missing = extract(&auth, Request::get("/").body(()).expect("request"))
            .await
            .expect_err("no session");
        assert_eq!(missing.code(), "invalid_session");
        let unknown = extract(
            &auth,
            Request::get("/")
                .header(SESSION_ID_HEADER, "not-a-session")
                .body(())
                .expect("request"),
        )
        .await
        .expect_err("unknown session");
        assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(unknown.code(), "invalid_session");
    }

    #[tokio::test]
    async fn session_auth_cookie_mutation_requires_trusted_origin() {
        let auth = auth_state().await;
        let session = session(&auth).await;
        let cookie = format!("waddle_session={}", session.id);
        let request = |method: Method, origin: Option<&str>| {
            let mut builder = Request::builder()
                .method(method)
                .uri("/")
                .header(header::COOKIE, &cookie);
            if let Some(origin) = origin {
                builder = builder.header(header::ORIGIN, origin);
            }
            builder.body(()).expect("request")
        };

        for origin in [None, Some("https://attacker.example"), Some("null")] {
            let error = extract(&auth, request(Method::DELETE, origin))
                .await
                .expect_err("untrusted origin");
            assert_eq!(error.status(), StatusCode::FORBIDDEN);
            assert_eq!(error.code(), "untrusted_origin");
        }
        let principal = extract(&auth, request(Method::POST, Some("http://localhost:3000")))
            .await
            .expect("trusted origin");
        assert_eq!(principal.transport, SessionTransport::Cookie);
        extract(&auth, request(Method::GET, None))
            .await
            .expect("safe methods need no origin");
    }
}
