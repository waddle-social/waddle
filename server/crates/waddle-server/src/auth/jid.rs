use crate::auth::AuthError;

/// Normalize a username into an immutable XMPP-safe localpart.
pub fn username_to_localpart(username: &str) -> String {
    let lower = username.trim().to_lowercase();
    let mut out = String::with_capacity(lower.len());

    for ch in lower.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '_' || ch == '-' {
            out.push(ch);
        }
    }

    if out.is_empty() {
        "user".to_string()
    } else {
        out
    }
}

/// Build bare JID from a localpart and domain.
pub fn localpart_to_jid(localpart: &str, domain: &str) -> Result<String, AuthError> {
    let localpart = username_to_localpart(localpart);
    let domain = domain.trim().to_lowercase();
    if domain.is_empty() {
        return Err(AuthError::InvalidRequest(
            "xmpp domain cannot be empty".to_string(),
        ));
    }
    Ok(format!("{}@{}", localpart, domain))
}

/// Parse a JID string and return localpart.
pub fn jid_to_localpart(jid: &str) -> Result<String, AuthError> {
    let localpart = jid
        .split_once('@')
        .map(|(local, _)| local)
        .unwrap_or(jid)
        .trim();

    if localpart.is_empty() {
        return Err(AuthError::InvalidRequest(
            "invalid jid localpart".to_string(),
        ));
    }

    Ok(localpart.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_normalization() {
        assert_eq!(username_to_localpart("Alice+Dev"), "alicedev");
        assert_eq!(username_to_localpart("FOO.BAR"), "foo.bar");
    }

    #[test]
    fn jid_roundtrip_localpart() {
        let jid = localpart_to_jid("alice", "example.com").unwrap();
        assert_eq!(jid, "alice@example.com");
        assert_eq!(jid_to_localpart(&jid).unwrap(), "alice");
    }
}
