use thiserror::Error;
use waddle_xmpp::ingress::ProtocolEpoch;

use crate::{db::DatabaseError, ingress_substrate::IngressSubstrateError};

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
    Database(#[from] DatabaseError),
    #[error(transparent)]
    Substrate(#[from] IngressSubstrateError),
    #[cfg(feature = "clustering")]
    #[error("this unit of work has no bound canonical node identity")]
    NodeIdentityUnbound,
    #[cfg(feature = "clustering")]
    #[error("the exact SM ownership claim is not held")]
    ClaimFenceMissing,
    #[cfg(feature = "clustering")]
    #[error("the SM stream does not exist")]
    StreamMissing,
    #[cfg(feature = "clustering")]
    #[error("the offered SM handled frontier is not the next wrapping value")]
    FrontierStale { stored: u32, offered: u32 },
}
