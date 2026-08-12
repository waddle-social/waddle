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

/// Opaque storage representation of a [`NormalizedTarget`].
///
/// The discriminator and JID text travel together so repository code cannot
/// separate or recombine them untyped; the raw parts are only readable at
/// the driver binding edge via the accessors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedTargetStorage {
    kind: i32,
    jid: String,
}

impl NormalizedTargetStorage {
    /// Storage discriminator: 0 = absent, 1 = bare, 2 = full.
    pub fn kind(&self) -> i32 {
        self.kind
    }

    /// JID text; empty exactly when the target is absent.
    pub fn jid(&self) -> &str {
        &self.jid
    }
}

impl NormalizedTarget {
    /// Encode this target in the Postgres ingress-alias representation.
    ///
    /// The tag order is frozen to SemanticDigest v1's target encoding:
    /// absent, bare, then full.  An absent target is stored with an empty
    /// value so the database's paired-kind CHECK remains total.
    pub fn to_storage(&self) -> NormalizedTargetStorage {
        let (kind, jid) = match self {
            Self::Absent => (0, String::new()),
            Self::Bare(jid) => (1, jid.to_string()),
            Self::Full(jid) => (2, jid.to_string()),
        };
        NormalizedTargetStorage { kind, jid }
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
