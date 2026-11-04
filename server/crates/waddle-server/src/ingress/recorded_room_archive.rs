//! Closed, startup-unwired writer for a recorded room archive obligation.
//!
//! The writer owns its UoW and never lends a transaction or accepts an archive
//! projection. Its input identifies already-recorded authority; the payload,
//! timestamp and archive-only sender metadata are reconstructed under lock.
//! This does not activate the relation-only epoch guard experiment.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use thiserror::Error;
use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent, MessageKey};
use waddle_xmpp::mam::{ArchiveExpectation, ArchiveOrdinal, MamTxStoreOutcome};
use waddle_xmpp::muc::RoomClaimFenceContext;
use waddle_xmpp::ownership::{Entity, EntityType, SharedNodeIdentity};
use waddle_xmpp_core::xep0359::StanzaId;

use crate::config::LineageConfig;
use crate::db::physical_attestation::AttestedPgPool;
use crate::db::Database;
use crate::ingress_substrate::MessageEnvelope;
use crate::ingress_uow::{
    CanonicalMessageRepository, ClaimRepository, EffectIntentRepository, EffectReceiptRepository,
    IngressUnitOfWork, IngressUowError, IngressUowTransaction, MamArchiveRepository,
};

/// Upper bounds cover checkout, database locks/statements and the entire
/// operation. The UoW may tighten the PostgreSQL lock and statement limits.
#[derive(Clone, Copy, Debug)]
pub struct RecordedRoomArchiveBounds {
    lock: Duration,
    statement: Duration,
    operation: Duration,
}

impl RecordedRoomArchiveBounds {
    pub fn new(
        lock: Duration,
        statement: Duration,
        operation: Duration,
    ) -> Result<Self, RecordedRoomArchiveError> {
        let valid_pg_timeout = |value: Duration| {
            let millis = value.as_millis();
            millis > 0 && millis <= i32::MAX as u128
        };
        if !valid_pg_timeout(lock)
            || !valid_pg_timeout(statement)
            || operation.is_zero()
            || tokio::time::Instant::now().checked_add(operation).is_none()
        {
            return Err(RecordedRoomArchiveError::InvalidBounds);
        }
        Ok(Self {
            lock,
            statement,
            operation,
        })
    }
}

/// An error after commit dispatch cannot establish whether the archive committed.
#[derive(Debug, Error)]
pub enum RecordedRoomArchiveError {
    #[error("invalid room archive transaction bounds")]
    InvalidBounds,
    #[error("room archive transaction preparation failed")]
    Preparation(#[source] IngressUowError),
    #[error("room archive transaction failed")]
    Operation(#[source] IngressUowError),
    #[error("recorded room archive provenance is missing or inconsistent")]
    Ineligible,
    #[error("room archive was suppressed by a tombstone or retention")]
    Suppressed,
    #[error("room archive transaction timed out before commit")]
    Timeout,
    #[error("room archive commit outcome is indeterminate")]
    CommitIndeterminate,
}

/// Complete, private PostgreSQL transaction authority. It is not wired into
/// startup; possession of this value never exposes the UoW, pool or SQL.
pub struct RecordedRoomArchiveWriter {
    uow: IngressUnitOfWork,
    bounds: RecordedRoomArchiveBounds,
}

impl RecordedRoomArchiveWriter {
    /// Consume the physically attested pool and bind the canonical node
    /// identity. The pool must have exactly the sum of the admission budgets.
    pub async fn from_attested_pool(
        pool: AttestedPgPool,
        lineage: LineageConfig,
        identity: SharedNodeIdentity,
        main_budget: u32,
        ingress_budget: u32,
        bounds: RecordedRoomArchiveBounds,
    ) -> Result<Self, RecordedRoomArchiveError> {
        let database = Database::from_attested_postgres_pool_with_ingress(
            "recorded-room-archive",
            pool,
            main_budget,
            ingress_budget,
        )
        .await
        .map_err(|error| RecordedRoomArchiveError::Preparation(error.into()))?;
        let uow =
            IngressUnitOfWork::open_with_node_identity(database.ingress_lane(), lineage, identity)
                .map_err(RecordedRoomArchiveError::Preparation)?;
        Ok(Self { uow, bounds })
    }

    /// Commit exactly one recorded room archive and its exact receipt. No
    /// retries or reconciliation run after an uncertain commit. A deadline
    /// drops the commit future and its local identity guard; the PostgreSQL
    /// claim remains locked only while that transaction is still active. An
    /// indeterminate result does not prove when either event occurred.
    pub async fn write(
        &self,
        key: MessageKey,
        stanza_id: &StanzaId,
        fence: &RoomClaimFenceContext,
    ) -> Result<ArchiveOrdinal, RecordedRoomArchiveError> {
        let commit_dispatched = AtomicBool::new(false);
        let work = async {
            let mut tx = self
                .uow
                .begin_with_timeouts(self.bounds.lock, self.bounds.statement)
                .await
                .map_err(RecordedRoomArchiveError::Preparation)?;
            let ordinal = write_in_transaction(&mut tx, key, stanza_id, fence).await?;
            // Set before polling COMMIT, including if the deadline fires first.
            commit_dispatched.store(true, Ordering::Relaxed);
            tx.commit()
                .await
                .map_err(|_| RecordedRoomArchiveError::CommitIndeterminate)?;
            Ok(ordinal)
        };
        match tokio::time::timeout(self.bounds.operation, work).await {
            Ok(result) => result,
            Err(_) if commit_dispatched.load(Ordering::Relaxed) => {
                Err(RecordedRoomArchiveError::CommitIndeterminate)
            }
            Err(_) => Err(RecordedRoomArchiveError::Timeout),
        }
    }
}

async fn write_in_transaction(
    tx: &mut IngressUowTransaction<'_>,
    key: MessageKey,
    stanza_id: &StanzaId,
    fence: &RoomClaimFenceContext,
) -> Result<ArchiveOrdinal, RecordedRoomArchiveError> {
    let room = stanza_id.by.to_bare();
    if stanza_id.by != room || fence.entity != Entity::new(EntityType::RoomActor, room.to_string())
    {
        return Err(RecordedRoomArchiveError::Ineligible);
    }
    let proof = ClaimRepository::assert_room_claim(tx, &room, &fence.owner, fence.epoch)
        .await
        .map_err(RecordedRoomArchiveError::Operation)?;
    if !CanonicalMessageRepository::lock(tx, key)
        .await
        .map_err(RecordedRoomArchiveError::Operation)?
    {
        return Err(RecordedRoomArchiveError::Ineligible);
    }
    let envelope = CanonicalMessageRepository::load_envelope(tx, key)
        .await
        .map_err(RecordedRoomArchiveError::Operation)?
        .ok_or(RecordedRoomArchiveError::Ineligible)?;
    let recorded = EffectIntentRepository::load(tx, key)
        .await
        .map_err(RecordedRoomArchiveError::Operation)?;
    let (authority, projection, expectation) = projection(&envelope, &recorded, stanza_id)?;
    MamArchiveRepository::lock_sequences(tx, std::slice::from_ref(&room))
        .await
        .map_err(RecordedRoomArchiveError::Operation)?;
    let outcome = MamArchiveRepository::store_fenced(tx, &proof, &room, &projection, expectation)
        .await
        .map_err(RecordedRoomArchiveError::Operation)?;
    let ordinal = match &outcome {
        MamTxStoreOutcome::Inserted { ordinal, .. }
        | MamTxStoreOutcome::Existing { ordinal, .. }
        | MamTxStoreOutcome::Repaired { ordinal, .. } => *ordinal,
        MamTxStoreOutcome::TombstoneHit(_) | MamTxStoreOutcome::Expired(_) => {
            return Err(RecordedRoomArchiveError::Suppressed)
        }
    };
    super::archive_authority::finalize(tx, key, &recorded, &room, &projection, &outcome)
        .await
        .map_err(RecordedRoomArchiveError::Operation)?;
    let receipt = super::receipt_key(authority).map_err(RecordedRoomArchiveError::Operation)?;
    EffectReceiptRepository::record_receipt(tx, key, receipt.kind, &receipt.semantic_identity_hash)
        .await
        .map_err(RecordedRoomArchiveError::Operation)?;
    Ok(ordinal)
}

fn projection<'a>(
    envelope: &MessageEnvelope,
    recorded: &'a [IngressEffectIntent],
    stanza_id: &StanzaId,
) -> Result<
    (
        &'a IngressEffectIntent,
        waddle_xmpp::mam::ArchivedMessage,
        ArchiveExpectation,
    ),
    RecordedRoomArchiveError,
> {
    let room = stanza_id.by.to_bare();
    // Finalization matches archive and opaque ID. Refuse any second intent
    // with those coordinates, even when its stanza issuer differs, so it
    // cannot accidentally acquire this archive's ordinal.
    let matching_ids = recorded
        .iter()
        .filter(|intent| {
            matches!(intent,
            IngressEffectIntent::ArchiveAuthoritative { archive, stanza_id: saved, .. }
            | IngressEffectIntent::SystemMessageArchive { archive, stanza_id: saved, .. }
            if archive == &room && saved.id == stanza_id.id)
        })
        .count();
    if matching_ids != 1 {
        return Err(RecordedRoomArchiveError::Ineligible);
    }
    let mut archives = recorded.iter().filter(|intent| {
        matches!(intent,
        IngressEffectIntent::ArchiveAuthoritative { archive, by, stanza_id: saved, .. }
            if archive == &room && by == &room && saved == stanza_id && saved.by == room)
    });
    let authority = archives
        .next()
        .ok_or(RecordedRoomArchiveError::Ineligible)?;
    if archives.next().is_some() {
        return Err(RecordedRoomArchiveError::Ineligible);
    }
    let IngressEffectIntent::ArchiveAuthoritative {
        archived_at,
        ordinal,
        ..
    } = authority
    else {
        return Err(RecordedRoomArchiveError::Ineligible);
    };
    let mut routes = recorded.iter().filter(|intent| matches!(intent,
        IngressEffectIntent::RouteMucGroupchat { room: saved, route_identity: EffectMessageIdentity::StanzaId(id), .. }
            if saved == &room && id == stanza_id));
    let route = routes.next().ok_or(RecordedRoomArchiveError::Ineligible)?;
    if routes.next().is_some() {
        return Err(RecordedRoomArchiveError::Ineligible);
    }
    let source = super::room_canonical::source(envelope, route)
        .map_err(|_| RecordedRoomArchiveError::Ineligible)?;
    let context = envelope
        .archive_context(stanza_id)
        .ok_or(RecordedRoomArchiveError::Ineligible)?;
    let message = crate::server::routes::interpret::project_groupchat_archive(
        &room,
        source,
        stanza_id.id.clone(),
        *archived_at,
        context.nickname_generation,
        context.sender.as_ref(),
    )
    .ok_or(RecordedRoomArchiveError::Ineligible)?;
    let expectation = ArchiveExpectation::Existing {
        stanza_id: stanza_id.clone(),
        archived_at: *archived_at,
        ordinal: *ordinal,
    };
    Ok((authority, message, expectation))
}
