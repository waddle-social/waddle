use waddle_xmpp::identity::{CanonicalBareJid, CanonicalDomain};

use crate::auth::AuthError;

/// The longest name [`username_to_localpart`] produces, leaving room for the
/// numeric suffix provisioning appends to resolve a clash within the account
/// profile's 64-byte limit.
pub const ACCOUNT_SLUG_MAX_BYTES: usize = 60;

/// Derive a name for a new account from a free-form name such as an OIDC
/// `preferred_username`: lowercase ASCII letters and digits, `.`, `_` and
/// `-`, at most [`ACCOUNT_SLUG_MAX_BYTES`] bytes, so the result is always in
/// the account profile. Other characters are dropped.
///
/// This generates names; it is lossy and never resolves an existing account.
pub fn username_to_localpart(username: &str) -> String {
    let mut out = String::with_capacity(username.len().min(ACCOUNT_SLUG_MAX_BYTES));
    for ch in username.trim().chars() {
        if out.len() == ACCOUNT_SLUG_MAX_BYTES {
            break;
        }
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-' {
            out.push(ch);
        }
    }

    if out.is_empty() {
        "user".to_string()
    } else {
        out
    }
}

/// The bare JID of the account whose stored localpart is `localpart` on the
/// server `domain`.
///
/// Both parts are taken exactly under the identity profile: a stored
/// localpart that preparation would change is an error, never rewritten, so
/// an account is always bound to the address its stored identity names.
pub fn localpart_to_jid(localpart: &str, domain: &str) -> Result<String, AuthError> {
    let domain = CanonicalDomain::parse_exact(domain)
        .ok()
        .filter(|canonical| canonical.as_str() == domain)
        .ok_or_else(|| {
            AuthError::InvalidRequest("xmpp domain is not in canonical form".to_string())
        })?;
    CanonicalBareJid::from_parts(localpart, &domain)
        .map(|jid| jid.to_string())
        .map_err(|error| {
            AuthError::InvalidRequest(format!("account localpart is not canonical: {error}"))
        })
}

/// Parse a JID string and return localpart.
#[cfg(test)]
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
        assert_eq!(username_to_localpart("Øyvind"), "yvind");
        assert_eq!(username_to_localpart("José"), "jos");
        assert_eq!(username_to_localpart("ＡＢＣ"), "user");
        assert_eq!(
            username_to_localpart(&"a".repeat(80)).len(),
            ACCOUNT_SLUG_MAX_BYTES
        );
    }

    #[test]
    fn stored_localparts_are_bound_exactly() {
        assert_eq!(
            localpart_to_jid("alice", "example.com").unwrap(),
            "alice@example.com"
        );
        assert!(localpart_to_jid("Alice", "example.com").is_err());
        assert!(localpart_to_jid("\u{fb01}x", "example.com").is_err());
        assert!(localpart_to_jid("alice", "Example.com").is_err());
    }

    #[test]
    fn jid_roundtrip_localpart() {
        let jid = localpart_to_jid("alice", "example.com").unwrap();
        assert_eq!(jid, "alice@example.com");
        assert_eq!(jid_to_localpart(&jid).unwrap(), "alice");
    }
}
