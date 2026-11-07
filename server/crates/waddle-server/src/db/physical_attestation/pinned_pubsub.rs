//! Unselected PubSub profile diagnostic on one pinned read-only transaction.
//!
//! The profile is partial. It is not a store capability or proof that the
//! complete role graph and old writers are fenced. The caller must separately
//! enforce quiescence of privileged catalog and ACL mutators, uncooperative
//! writers, and old backends throughout the observation. The advisory fence
//! coordinates only participants; the v3 handoff is an operator procedure.

use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::sync::{oneshot, Semaphore};
use tokio::time::Instant;

use super::pinned_entry::{prepare_read_only_entry, DarkPinnedEntryError, ENTRY_DEADLINE};
use super::{
    read_epoch_after_prelock, AttestationMode, AttestedPgPool, DarkIngressAuthorityError,
    PinnedRuntimeAuthority, CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use crate::db::{Database, DatabaseError, Transaction};
use crate::pubsub::runtime_attach::verify_pinned_restricted_v2_tx;
use crate::pubsub::{PubSubRuntimeAttachError, RestrictedServingProfileV2};

const RETRY_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Debug, Error)]
pub enum PinnedPubSubProfileError {
    #[error("PubSub profile requires one pinned pool and consistent independent object pins")]
    InvalidPins,
    #[error("PubSub profile diagnostic is at its bounded concurrency limit")]
    Overloaded,
    #[error("PubSub profile diagnostic requires a Tokio runtime")]
    RuntimeUnavailable,
    #[error("the PubSub diagnostic worker did not return a result")]
    WorkerLost,
    #[error("the PubSub diagnostic timed out")]
    Timeout,
    #[error("PubSub diagnostic rollback was not acknowledged")]
    RollbackUnconfirmed,
    #[error("the PubSub diagnostic test pause was cancelled")]
    PauseCancelled,
    #[error(transparent)]
    Entry(#[from] DarkPinnedEntryError),
    #[error(transparent)]
    Epoch(#[from] DarkIngressAuthorityError),
    #[error(transparent)]
    PubSub(#[from] PubSubRuntimeAttachError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("PubSub diagnostic PostgreSQL operation failed")]
    Postgres(#[from] sqlx::Error),
}

/// Owns the exact pinned pool and independent profile pins. No pool,
/// connection, transaction, callback, or reusable proof can be extracted.
/// A success is only a completed read-only diagnostic, never permission to
/// attach or serve a store.
pub struct PinnedPubSubProfile {
    pool: AttestedPgPool,
    profile: RestrictedServingProfileV2,
    admission: Arc<Semaphore>,
}

/// Receives an owned worker's result. Dropping this receiver does not cancel
/// the worker. Runtime shutdown or a backend failure is not rollback proof.
pub struct PinnedPubSubProfileJob {
    result: oneshot::Receiver<Result<(), PinnedPubSubProfileError>>,
}

struct ProfilePause {
    ready: oneshot::Sender<(i32, bool, bool)>,
    release: oneshot::Receiver<()>,
}

impl PinnedPubSubProfileJob {
    pub async fn finish(self) -> Result<(), PinnedPubSubProfileError> {
        self.result
            .await
            .map_err(|_| PinnedPubSubProfileError::WorkerLost)?
    }
}

impl PinnedPubSubProfile {
    pub fn new(
        pool: AttestedPgPool,
        profile: RestrictedServingProfileV2,
    ) -> Result<Self, PinnedPubSubProfileError> {
        let AttestationMode::Pinned(authority) = &pool.writer_capability.mode else {
            return Err(PinnedPubSubProfileError::InvalidPins);
        };
        let mut relations = vec![
            authority.attestor.lineage_relation_oid,
            authority.attestor.lineage_primary_index_oid,
            authority.ledger.ledger_relation_oid,
            authority.ledger.ledger_primary_index_oid,
            authority.epoch_relation_oid,
            authority.epoch_primary_index_oid,
            authority.guard_manifest_relation_oid,
        ];
        relations.extend_from_slice(&profile.v1.helper_relation_oids);
        relations.push(profile.effect_relation_oid);
        relations.extend_from_slice(&profile.mam_relation_oids);
        if profile.epoch_relation_oid != authority.epoch_relation_oid
            || profile.v1.helper_function_oid == 0
            || profile.v1.helper_function_oid == authority.attestor.attestor_function_oid
            || relations.contains(&0)
            || relations
                .iter()
                .enumerate()
                .any(|(index, oid)| relations[..index].contains(oid))
            || pool.max_connections() == 0
        {
            return Err(PinnedPubSubProfileError::InvalidPins);
        }
        let admission = Arc::new(Semaphore::new(pool.max_connections() as usize));
        Ok(Self {
            pool,
            profile,
            admission,
        })
    }

    pub(crate) fn start(&self) -> Result<PinnedPubSubProfileJob, PinnedPubSubProfileError> {
        self.start_inner(None)
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn start_paused_for_test(
        &self,
        ready: oneshot::Sender<(i32, bool, bool)>,
        release: oneshot::Receiver<()>,
    ) -> Result<PinnedPubSubProfileJob, PinnedPubSubProfileError> {
        self.start_inner(Some(ProfilePause { ready, release }))
    }

    fn start_inner(
        &self,
        pause: Option<ProfilePause>,
    ) -> Result<PinnedPubSubProfileJob, PinnedPubSubProfileError> {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| PinnedPubSubProfileError::RuntimeUnavailable)?;
        let permit = Arc::clone(&self.admission)
            .try_acquire_owned()
            .map_err(|_| PinnedPubSubProfileError::Overloaded)?;
        let AttestationMode::Pinned(authority) = &self.pool.writer_capability.mode else {
            return Err(PinnedPubSubProfileError::InvalidPins);
        };
        let authority = (**authority).clone();
        let profile = self.profile.clone();
        let database =
            Database::from_postgres_pool("pinned PubSub profile", self.pool.pool.clone());
        let (sender, result) = oneshot::channel();
        drop(runtime.spawn(async move {
            let outcome = inspect(&database, &authority, &profile, pause).await;
            // Completion must not wake the caller before admission is free.
            drop(permit);
            let _ = sender.send(outcome);
        }));
        Ok(PinnedPubSubProfileJob { result })
    }
}

enum Inspection {
    Complete,
    Retry,
}

async fn inspect(
    database: &Database,
    authority: &PinnedRuntimeAuthority,
    profile: &RestrictedServingProfileV2,
    mut pause: Option<ProfilePause>,
) -> Result<(), PinnedPubSubProfileError> {
    let deadline = Instant::now() + ENTRY_DEADLINE;
    loop {
        let mut tx = tokio::time::timeout_at(deadline, database.begin())
            .await
            .map_err(|_| PinnedPubSubProfileError::Timeout)??;
        let outcome = tokio::time::timeout_at(
            deadline,
            inspect_transaction(&mut tx, authority, profile, &mut pause),
        )
        .await
        .unwrap_or(Err(PinnedPubSubProfileError::Timeout));

        // The detached worker, not the receiver, owns cleanup. It retains its
        // admission permit and exact lease while rollback is pending. A failed
        // acknowledgement is unconfirmed; a stalled connection cannot be
        // reported as a successful result.
        tx.rollback()
            .await
            .map_err(|_| PinnedPubSubProfileError::RollbackUnconfirmed)?;
        match outcome? {
            Inspection::Complete => return Ok(()),
            Inspection::Retry => {
                tokio::time::timeout_at(deadline, tokio::time::sleep(RETRY_INTERVAL))
                    .await
                    .map_err(|_| PinnedPubSubProfileError::Timeout)?;
            }
        }
    }
}

async fn inspect_transaction(
    tx: &mut Transaction<'_>,
    authority: &PinnedRuntimeAuthority,
    profile: &RestrictedServingProfileV2,
    pause: &mut Option<ProfilePause>,
) -> Result<Inspection, PinnedPubSubProfileError> {
    // SET TRANSACTION does not take a snapshot. The try-lock is the first
    // snapshot-bearing statement; do not wait on contention with that
    // snapshot, or acquire a later-order barrier before this fence.
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await?;
    let connection = tx
        .postgres_connection()
        .ok_or(PinnedPubSubProfileError::InvalidPins)?;
    let acquired: bool =
        sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)")
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(connection)
            .await?;
    if !acquired {
        return Ok(Inspection::Retry);
    }
    prepare_read_only_entry(tx, authority, None).await?;
    let connection = tx
        .postgres_connection()
        .ok_or(PinnedPubSubProfileError::InvalidPins)?;
    let identity = authority.config.postgres();
    read_epoch_after_prelock(connection, &identity.schema.name.0, identity.schema.oid).await?;
    verify_pinned_restricted_v2_tx(
        tx,
        identity,
        authority.config.definer_role.as_str(),
        profile,
    )
    .await?;

    if let Some(pause) = pause.take() {
        let connection = tx
            .postgres_connection()
            .ok_or(PinnedPubSubProfileError::InvalidPins)?;
        let observation: (i32, bool, bool) = sqlx::query_as(
            "SELECT pg_catalog.pg_backend_pid(), \
             pg_catalog.current_setting('transaction_read_only') = 'on', \
             pg_catalog.current_setting('transaction_isolation') = 'read committed'",
        )
        .fetch_one(connection)
        .await?;
        let _ = pause.ready.send(observation);
        pause
            .release
            .await
            .map_err(|_| PinnedPubSubProfileError::PauseCancelled)?;
    }
    Ok(Inspection::Complete)
}
