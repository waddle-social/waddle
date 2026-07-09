use tracing::{debug, warn};
use waddle_xmpp::{Session as XmppSession, XmppError};

use crate::auth::jid::jid_to_localpart;
use crate::auth::{localpart_to_jid, AuthError, SessionManager};

pub(crate) async fn validate_session(
    session_manager: &SessionManager,
    jid: &jid::Jid,
    token: &str,
) -> Result<XmppSession, XmppError> {
    debug!("Validating XMPP session");

    let expected_localpart = jid_to_localpart(&jid.to_string()).map_err(|e| {
        warn!("Failed to extract localpart from JID");
        XmppError::auth_failed(format!("Invalid JID format: {}", e))
    })?;

    let session = validate_http_session(session_manager, token).await?;

    if session.xmpp_localpart != expected_localpart {
        warn!("Localpart mismatch between JID and session");
        return Err(XmppError::auth_failed("JID does not match session"));
    }

    Ok(XmppSession {
        user_id: session.user_jid,
        jid: jid.to_bare(),
        created_at: session.created_at,
        expires_at: session_expires_at(session.expires_at),
    })
}

pub(crate) async fn validate_session_token(
    session_manager: &SessionManager,
    domain: &str,
    token: &waddle_xmpp::auth::oauthbearer::OAuthBearerToken,
) -> Result<XmppSession, XmppError> {
    debug!("Validating XMPP session token (OAUTHBEARER)");

    let session = validate_http_session(session_manager, token.expose_secret()).await?;

    let jid_str = localpart_to_jid(&session.xmpp_localpart, domain).map_err(|e| {
        warn!("Failed to convert session localpart to JID");
        XmppError::auth_failed(format!("Invalid localpart format: {}", e))
    })?;

    let bare_jid: jid::BareJid = jid_str.parse().map_err(|e| {
        warn!("Failed to parse generated JID");
        XmppError::auth_failed(format!("Invalid JID: {:?}", e))
    })?;

    debug!("OAUTHBEARER session validated");

    Ok(XmppSession {
        user_id: session.user_jid,
        jid: bare_jid,
        created_at: session.created_at,
        expires_at: session_expires_at(session.expires_at),
    })
}

async fn validate_http_session(
    session_manager: &SessionManager,
    token: &str,
) -> Result<crate::auth::Session, XmppError> {
    session_manager.validate_session(token).await.map_err(|e| {
        warn!(
            error_kind = auth_error_kind(&e),
            "Session validation failed"
        );
        match e {
            AuthError::SessionNotFound(_) => XmppError::SessionNotFound,
            AuthError::SessionExpired => XmppError::SessionNotFound,
            _ => XmppError::auth_failed(format!("Session validation failed: {}", e)),
        }
    })
}

fn auth_error_kind(error: &AuthError) -> &'static str {
    match error {
        AuthError::SessionNotFound(_) => "not_found",
        AuthError::SessionExpired => "expired",
        _ => "internal",
    }
}

fn session_expires_at(
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
) -> chrono::DateTime<chrono::Utc> {
    expires_at.unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::hours(24))
}
