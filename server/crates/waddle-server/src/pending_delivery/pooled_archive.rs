//! Unwired pending-delivery resolver over the shared correctness pool.

use crate::ingress::{DarkSharedCorrectness, DarkSharedCorrectnessError};
use crate::pending_delivery::{ArchiveResolveError, ArchiveResolver};
use std::time::Duration;
use waddle_xmpp_core::xep0359::StanzaId;

const LOOKUP_DEADLINE: Duration = Duration::from_secs(15);

/// The only exposed operation ends its admitted read before decoding XML.
/// The underlying database and ingress unit of work cannot escape this type.
pub(crate) struct PooledArchiveResolver {
    shared: DarkSharedCorrectness,
    postgres: bool,
}

impl PooledArchiveResolver {
    pub(crate) fn new(shared: DarkSharedCorrectness) -> Self {
        let postgres = shared.uses_postgres();
        Self { shared, postgres }
    }

    pub(crate) async fn resolve_with_deadline(
        &self,
        stanza: &StanzaId,
        deadline: Duration,
    ) -> Result<Option<xmpp_parsers::message::Message>, ArchiveResolveError> {
        let operation = self.shared.lookup_archive_or_stanza_id(stanza);
        // An internal deadline could destroy the sole private-memory SQLite
        // connection while it is in flight. Only PostgreSQL gets this bound.
        let result = if self.postgres {
            match tokio::time::timeout(deadline, operation).await {
                Ok(result) => result,
                Err(_) => return Err(unavailable()),
            }
        } else {
            operation.await
        };
        let result = match result {
            Ok(row) => Ok(row),
            Err(DarkSharedCorrectnessError::Mam(error)) => Err(error),
            Err(_) => return Err(unavailable()),
        };
        super::flush::classify_archive_lookup(stanza, result)
    }
}

fn unavailable() -> ArchiveResolveError {
    ArchiveResolveError::Storage(waddle_xmpp::mam::MamStorageError::Database(
        "MAM lookup authority is unavailable".to_owned(),
    ))
}

#[async_trait::async_trait]
impl ArchiveResolver for PooledArchiveResolver {
    async fn resolve(
        &self,
        stanza: &StanzaId,
    ) -> Result<Option<xmpp_parsers::message::Message>, ArchiveResolveError> {
        self.resolve_with_deadline(stanza, LOOKUP_DEADLINE).await
    }
}
