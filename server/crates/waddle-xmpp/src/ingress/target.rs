use crate::ingress::IngressTypeError;

/// The exact pre-routing addressed form parsed from a stanza.
///
/// `Absent` means the stanza had no `to` attribute at all. This is captured
/// before any routing rewrite that might supply the sender's bare JID as an
/// implicit target, so such a rewrite is never visible here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NormalizedTarget {
    Absent,
    Bare(jid::BareJid),
    Full(jid::FullJid),
}

impl NormalizedTarget {
    /// Encode this target in the Postgres ingress-alias representation.
    ///
    /// The tag order is frozen to SemanticDigest v1's target encoding:
    /// absent, bare, then full.  An absent target is stored with an empty
    /// value so the database's paired-kind CHECK remains total.
    pub fn to_storage(&self) -> (i32, String) {
        match self {
            Self::Absent => (0, String::new()),
            Self::Bare(jid) => (1, jid.to_string()),
            Self::Full(jid) => (2, jid.to_string()),
        }
    }

    /// Decode the Postgres ingress-alias representation without accepting
    /// malformed JIDs or unknown discriminants.
    pub fn from_storage(kind: i32, value: &str) -> Result<Self, IngressTypeError> {
        match kind {
            0 if value.is_empty() => Ok(Self::Absent),
            0 => Err(IngressTypeError::InvalidNormalizedTargetStorage { kind }),
            1 => value
                .parse::<jid::BareJid>()
                .map(Self::Bare)
                .map_err(|_| IngressTypeError::InvalidNormalizedTargetStorage { kind }),
            2 => value
                .parse::<jid::FullJid>()
                .map(Self::Full)
                .map_err(|_| IngressTypeError::InvalidNormalizedTargetStorage { kind }),
            _ => Err(IngressTypeError::InvalidNormalizedTargetStorage { kind }),
        }
    }
}
