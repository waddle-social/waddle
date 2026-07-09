use thiserror::Error;

use crate::ownership::Entity;

/// Errors that can occur during MAM storage operations.
#[derive(Error, Debug)]
pub enum MamStorageError {
    #[error("Database error: {0}")]
    Database(String),

    #[error("Message not found: {0}")]
    NotFound(String),

    #[error("Invalid query parameter: {0}")]
    InvalidQuery(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    /// ADR-0017 Phase 3 Slice 7 FIX 1 (council-adjudicated): a fenced
    /// [`super::MamStorage::store_message_fenced`] call's own
    /// `SELECT ... FOR SHARE` fencing check (against `clustering_claims`,
    /// mirroring `PendingStorageError::NotOwner`/`sm_persistence_fenced`'s
    /// identical pattern one table over) observed that this node does not
    /// hold the room's ownership claim at the epoch it believed was
    /// current. The write was rolled back before touching `mam_messages`.
    /// Only ever returned by a cluster-fenced implementation; the portable,
    /// single-node implementation has no fencing concept and never returns
    /// this.
    #[error(
        "fencing check failed: this node does not hold entity '{entity}' at the expected claim epoch"
    )]
    NotOwner { entity: Entity },

    /// A caller requested a clustered, ownership-fenced archive write from
    /// a storage implementation that cannot execute the fence. Falling back
    /// to an unfenced insert would let a stale room owner fan out messages,
    /// so this is a hard failure for the clustered groupchat path.
    #[error("clustered MAM fencing is unavailable for entity '{entity}'")]
    FencingUnavailable { entity: Entity },

    /// ADR-0017 Phase 3 Slice 7 FIX 1: clustered MAM fencing requires this
    /// storage's own database to be co-located with the clustering global
    /// database (the fencing `SELECT ... FOR SHARE` targets
    /// `clustering_claims`, which only exists there) — mirroring
    /// `PendingStorageError::ClusterColocationMismatch`'s identical
    /// invariant one table over. Both fields are expected to already be
    /// credential-redacted by the caller before construction.
    #[error(
        "clustered MAM fencing must be co-located with the clustering claims tables: resolved \
         MAM database URL ({mam_database_url}) does not match the clustering global database \
         URL ({global_database_url})"
    )]
    ClusterColocationMismatch {
        mam_database_url: String,
        global_database_url: String,
    },
}

impl From<sqlx::Error> for MamStorageError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}
