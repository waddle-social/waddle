use thiserror::Error;
use waddle_xmpp::ingress::ProtocolEpoch;

use crate::{
    db::DatabaseError, ingress_substrate::IngressSubstrateError, ingress_uow::DbRetryClass,
};
use waddle_xmpp::ingress::EffectIntentCodecError;

/// Fail-closed errors from the PostgreSQL ingress unit of work.
#[derive(Debug, Error)]
pub enum IngressUowError {
    #[error("ingress unit of work requires PostgreSQL")]
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
    #[error("stored shadow ingress frontier is malformed")]
    InvalidStoredShadowFrontier,
    #[error("shadow ingress stream is missing")]
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
    #[cfg(feature = "clustering")]
    #[error("the SM stream does not exist")]
    StreamMissing,
    #[cfg(feature = "clustering")]
    #[error("the offered SM handled frontier is not the next wrapping value")]
    FrontierStale { stored: u32, offered: u32 },
}

impl IngressUowError {
    pub fn retry_class(&self) -> DbRetryClass {
        match self {
            Self::Database { retry_class } => *retry_class,
            Self::Substrate(IngressSubstrateError::Database { retry_class }) => *retry_class,
            _ => DbRetryClass::NotRetryable,
        }
    }
}

impl From<DatabaseError> for IngressUowError {
    fn from(error: DatabaseError) -> Self {
        Self::Database {
            retry_class: DbRetryClass::from_database_error(&error),
        }
    }
}
