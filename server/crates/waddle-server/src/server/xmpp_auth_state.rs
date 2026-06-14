use tracing::{debug, warn};
use waddle_xmpp::{Session as XmppSession, XmppError};

use crate::auth::jid::jid_to_localpart;
use crate::auth::{localpart_to_jid, AuthError, SessionManager};

pub(crate) async fn validate_session(
    session_manager: &SessionManager,
    jid: &jid::Jid,
    token: &str,
) -> Result<XmppSession, XmppError> {
    debug!(jid = %jid, "Validating XMPP session");

    let expected_localpart = jid_to_localpart(&jid.to_string()).map_err(|e| {
        warn!(jid = %jid, error = %e, "Failed to extract localpart from JID");
        XmppError::auth_failed(format!("Invalid JID format: {}", e))
    })?;

    let session = validate_http_session(session_manager, token).await?;

    if session.xmpp_localpart != expected_localpart {
        warn!(
            expected_localpart = %expected_localpart,
            session_localpart = %session.xmpp_localpart,
            "Localpart mismatch between JID and session"
        );
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
    token: &str,
) -> Result<XmppSession, XmppError> {
    debug!(token_prefix = %&token[..token.len().min(8)], "Validating XMPP session token (OAUTHBEARER)");

    let session = validate_http_session(session_manager, token).await?;

    let jid_str = localpart_to_jid(&session.xmpp_localpart, domain).map_err(|e| {
        warn!(localpart = %session.xmpp_localpart, error = %e, "Failed to convert localpart to JID");
        XmppError::auth_failed(format!("Invalid localpart format: {}", e))
    })?;

    let bare_jid: jid::BareJid = jid_str.parse().map_err(|e| {
        warn!(jid = %jid_str, error = ?e, "Failed to parse generated JID");
        XmppError::auth_failed(format!("Invalid JID: {:?}", e))
    })?;

    debug!(jid = %bare_jid, user_jid = %session.user_jid, "OAUTHBEARER session validated");

    Ok(XmppSession {
        user_id: session.user_jid,
        jid: bare_jid,
        created_at: session.created_at,
        expires_at: session_expires_at(session.expires_at),
    })
}

pub(crate) fn oauth_discovery_url(domain: &str) -> String {
    let base_url =
        std::env::var("WADDLE_BASE_URL").unwrap_or_else(|_| format!("https://{}", domain));
    format!(
        "{}/.well-known/oauth-authorization-server",
        base_url.trim_end_matches('/')
    )
}

async fn validate_http_session(
    session_manager: &SessionManager,
    token: &str,
) -> Result<crate::auth::Session, XmppError> {
    session_manager.validate_session(token).await.map_err(|e| {
        warn!(token_prefix = %&token[..token.len().min(8)], error = %e, "Session validation failed");
        match e {
            AuthError::SessionNotFound(_) => XmppError::SessionNotFound,
            AuthError::SessionExpired => XmppError::SessionNotFound,
            _ => XmppError::auth_failed(format!("Session validation failed: {}", e)),
        }
    })
}

fn session_expires_at(
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
) -> chrono::DateTime<chrono::Utc> {
    expires_at.unwrap_or_else(|| chrono::Utc::now() + chrono::Duration::hours(24))
}
