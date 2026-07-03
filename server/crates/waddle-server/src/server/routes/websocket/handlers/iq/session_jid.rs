use super::*;

pub(super) fn session_bare_jid(
    session: &Session,
    user_domain: &str,
) -> Result<BareJid, PubSubError> {
    format!(
        "{}@{}",
        session.xmpp_localpart.to_ascii_lowercase(),
        user_domain.to_ascii_lowercase()
    )
    .parse::<BareJid>()
    .map_err(|_| PubSubError::InvalidJid)
}
