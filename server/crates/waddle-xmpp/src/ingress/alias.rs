use crate::ingress::digest::SemanticDigest;
use crate::ingress::keys::MessageKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasResolution {
    NoOrigin(MessageKey),
    Aliased(AliasOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasOutcome {
    Inserted(MessageKey),
    Existing(MessageKey),
    Conflict(AliasConflict),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasConflict {
    pub existing: MessageKey,
    pub stored: SemanticDigest,
    pub offered: SemanticDigest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAlias {
    pub key: MessageKey,
    pub digest: SemanticDigest,
}

/// Resolves an offered origin-id against an optionally stored alias.
pub fn resolve_alias(
    origin_present: bool,
    offered: &SemanticDigest,
    stored: Option<&StoredAlias>,
    mint: impl FnOnce() -> MessageKey,
) -> AliasResolution {
    if !origin_present {
        return AliasResolution::NoOrigin(mint());
    }

    match stored {
        None => AliasResolution::Aliased(AliasOutcome::Inserted(mint())),
        Some(stored) if stored.digest == *offered => {
            AliasResolution::Aliased(AliasOutcome::Existing(stored.key))
        }
        Some(stored) => AliasResolution::Aliased(AliasOutcome::Conflict(AliasConflict {
            existing: stored.key,
            stored: stored.digest.clone(),
            offered: offered.clone(),
        })),
    }
}
