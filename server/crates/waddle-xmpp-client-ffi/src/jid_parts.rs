use jid::Jid;

/// Parsed JID components, validated per RFC 7622 by the shared `jid` crate.
///
/// Exposed so native clients reuse the workspace JID parser instead of
/// hand-rolling string splits.
#[derive(uniffi::Record, Clone)]
pub struct WaddleJidParts {
    pub localpart: Option<String>,
    pub domain: String,
    pub resource: Option<String>,
}

/// Parse and validate a JID string. Returns `None` for invalid JIDs.
#[uniffi::export]
pub fn parse_jid(input: String) -> Option<WaddleJidParts> {
    let jid = Jid::new(input.trim()).ok()?;
    Some(WaddleJidParts {
        localpart: jid.node().map(|node| node.to_string()),
        domain: jid.domain().to_string(),
        resource: jid.resource().map(|resource| resource.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_jid() {
        let parts = parse_jid("alice@waddle.social/phone".into()).expect("valid jid");
        assert_eq!(parts.localpart.as_deref(), Some("alice"));
        assert_eq!(parts.domain, "waddle.social");
        assert_eq!(parts.resource.as_deref(), Some("phone"));
    }

    #[test]
    fn parses_bare_and_domain_jids() {
        let bare = parse_jid("alice@waddle.social".into()).expect("valid bare jid");
        assert_eq!(bare.localpart.as_deref(), Some("alice"));
        assert_eq!(bare.resource, None);

        let domain = parse_jid("muc.waddle.social".into()).expect("valid domain jid");
        assert_eq!(domain.localpart, None);
        assert_eq!(domain.domain, "muc.waddle.social");
    }

    #[test]
    fn trims_whitespace_and_rejects_invalid() {
        let parts = parse_jid("  alice@waddle.social  ".into()).expect("trimmed jid");
        assert_eq!(parts.domain, "waddle.social");

        assert!(parse_jid(String::new()).is_none());
        assert!(parse_jid("   ".into()).is_none());
        assert!(parse_jid("@waddle.social".into()).is_none());
        assert!(parse_jid("alice@".into()).is_none());
    }
}
