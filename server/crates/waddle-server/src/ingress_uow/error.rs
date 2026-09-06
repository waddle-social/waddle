use thiserror::Error;
use waddle_xmpp::ingress::ProtocolEpoch;

use crate::{
    db::DatabaseError, ingress_substrate::IngressSubstrateError, ingress_uow::DbRetryClass,
};
use waddle_xmpp::ingress::EffectIntentCodecError;

/// Fail-closed errors from the ingress unit of work.
#[derive(Debug, Error)]
pub enum IngressUowError {
    #[error("planned room delivery is missing its authoritative stanza-id")]
    MissingRoomStanzaId,
    #[error("ingress transaction timed out before commit")]
    Timeout,
    #[error("ingress authority has stopped")]
    AuthorityStopped,
    #[error("authenticated principal is no longer asserted")]
    PrincipalAssertionFailed,
    #[error("room snapshot generation is stale")]
    RoomGenerationStale,
    #[error("ingress frontier is stale")]
    IngressFrontierStale,
    #[error("commit acknowledgement was lost")]
    AmbiguousCommit,
    #[error("ingress transaction timeout bounds were not proven before taking locks")]
    TransactionBoundsUnproven,
    #[error("ingress inbox projection mutation does not apply to this durable effect")]
    UnsupportedInboxProjection,
    #[error("ingress tombstone payload could not be encoded")]
    TombstonePayloadEncoding,
    #[error("stored archive rich payload could not be decoded")]
    InvalidArchiveRichPayload,

    #[error("this operation requires single-node ingress fencing")]
    SingleNodeFencingRequired,
    #[cfg(feature = "clustering")]
    #[error("clustered ingress fencing requires PostgreSQL")]
    ClusteredFencingRequiresPostgres,
    #[error("this repository operation requires PostgreSQL")]
    PostgresRequired,
    #[error("live ingress protocol epoch exceeds what this binary supports")]
    EpochUnsupported {
        live: ProtocolEpoch,
        supported: ProtocolEpoch,
    },
    #[error("ingress epoch proof query returned no row")]
    EpochProofMissing,
    #[error("ingress unit of work lineage attestation failed")]
    Lineage(#[source] DatabaseError),
    #[error("ingress unit of work database operation failed")]
    Database { retry_class: DbRetryClass },
    #[error(transparent)]
    MamStore(#[from] waddle_xmpp::mam::MamTxStoreError),
    #[error(transparent)]
    Inbox(#[from] crate::inbox::InboxTxError),
    #[error(transparent)]
    Substrate(#[from] IngressSubstrateError),
    #[error("ingress effect-intent encoding failed")]
    EffectIntentCodec(#[from] EffectIntentCodecError),
    #[error("ingress effect-intent conflicts with an existing immutable row")]
    EffectIntentConflict,
    #[error("ingress effect-intent message row is missing")]
    EffectIntentMessageMissing,
    #[error("ingress effect-intent ordinal cannot be represented")]
    EffectIntentOrdinalOverflow,
    #[error("stored ingress stream identity is malformed")]
    InvalidStoredSmIngressId,
    #[error("stored ingress frontier is malformed")]
    InvalidStoredFrontier,
    #[error("ingress stream is missing")]
    SmIngressStreamMissing,
    #[error("authenticated principal reference cannot be represented in storage")]
    PrincipalReferenceOutOfRange,
    #[error("stored principal expiry is malformed")]
    InvalidStoredPrincipalExpiry,
    #[cfg(feature = "clustering")]
    #[error("this unit of work has no bound canonical node identity")]
    NodeIdentityUnbound,
    #[cfg(feature = "clustering")]
    #[error("the exact ownership claim is not held")]
    ClaimFenceMissing,
}

impl IngressUowError {
    pub fn retry_class(&self) -> DbRetryClass {
        match self {
            Self::Database { retry_class } => *retry_class,
            Self::Substrate(IngressSubstrateError::Database { retry_class }) => *retry_class,
            Self::MamStore(waddle_xmpp::mam::MamTxStoreError::Database(error)) => {
                DbRetryClass::from_sqlx_error(error)
            }
            Self::Inbox(crate::inbox::InboxTxError::Database(error)) => {
                DbRetryClass::from_database_error(error)
            }
            _ => DbRetryClass::NotRetryable,
        }
    }
}

impl From<DatabaseError> for IngressUowError {
    fn from(error: DatabaseError) -> Self {
        if super::retry::is_database_timeout(&error) {
            return Self::Timeout;
        }
        Self::Database {
            retry_class: DbRetryClass::from_database_error(&error),
        }
    }
}
