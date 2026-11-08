//! Closed read-only selected-transaction candidate guarded against cooperative
//! catalog mutation. P21 receipt writes need independently provisioned
//! `ReceiptOperationPins`, the A-owner V1030 receipt profile, and in-transaction
//! exact journal/function verification; the existing `ReceiptOperationAuthority`
//! starts its own transaction and cannot be composed here. This candidate is
//! read-only and is not selected by server startup. The lock order is catalog
//! fence, attestor lineage barrier, migration-ledger barrier, ledger relation,
//! epoch relation, then guard-manifest relation. Cooperative mutators must take
//! the exclusive catalog fence before any of those subordinate locks.

use std::ops::{Deref, DerefMut};
use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::postgres::PgConnection;
use sqlx::{Connection, Postgres, Transaction};
use thiserror::Error;
use waddle_xmpp::ingress::ProtocolEpoch;

use super::pinned_catalog::verify_pinned_epoch_manifest_catalog_sqlx;
use super::preflight::{
    attest_dark_catalog_pinned, attest_dark_catalog_pinned_with_partial_profile,
    DarkPhysicalAttestationError,
};
use super::DarkIngressAuthorityError;
use super::{
    read_epoch_after_prelock, AttestationMode, AttestedPgPool, PhysicalAttestationError,
    PinnedCatalogError, PinnedPartialProfileError, PinnedPartialProfilePinsV1,
    PinnedPushProfileError, PinnedPushProfilePinsV1, PinnedRuntimeAuthority,
    CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use crate::db::migrations::{
    verify_dark_shared_migration_ledger_v1030_partial_profile_sqlx,
    verify_dark_shared_migration_ledger_v1030_read_sqlx, DarkLedgerPreflightError,
};

const ENTRY_DEADLINE: Duration = Duration::from_secs(20);
const RETRY_INTERVAL: Duration = Duration::from_millis(20);
type EntryPause = (
    tokio::sync::oneshot::Sender<i32>,
    tokio::sync::oneshot::Receiver<()>,
);

#[derive(Debug, Error)]
pub enum PinnedSelectedEntryError {
    #[error("selected transaction entry requires the pinned PostgreSQL pool")]
    NotPinned,
    #[error("selected transaction outcome is unconfirmed; reconcile backend {backend_pid:?}")]
    ObservationUnconfirmed { backend_pid: Option<i32> },
    #[error("selected transaction test pause was cancelled")]
    PauseCancelled,
    #[error(transparent)]
    Attestation(#[from] DarkPhysicalAttestationError),
    #[error(transparent)]
    Ledger(#[from] DarkLedgerPreflightError),
    #[error(transparent)]
    Catalog(#[from] PinnedCatalogError),
    #[error(transparent)]
    PartialProfile(#[from] PinnedPartialProfileError),
    #[error(transparent)]
    PushProfile(#[from] PinnedPushProfileError),
    #[error(transparent)]
    Epoch(#[from] DarkIngressAuthorityError),
    #[error(transparent)]
    Physical(#[from] PhysicalAttestationError),
    #[error("selected transaction PostgreSQL operation failed")]
    Postgres(#[from] sqlx::Error),
}

/// A closed API over one exact pinned pool. It exposes a typed protocol epoch
/// and named partial observations; no SQL, connection, transaction, callback,
/// or pool accessor escapes. It does not claim all-store authority or select
/// production serving.
pub struct PinnedSelectedEntry {
    pool: AttestedPgPool,
}

impl PinnedSelectedEntry {
    pub fn new(pool: AttestedPgPool) -> Result<Self, PinnedSelectedEntryError> {
        if !matches!(&pool.writer_capability.mode, AttestationMode::Pinned(_)) {
            return Err(PinnedSelectedEntryError::NotPinned);
        }
        Ok(Self { pool })
    }

    /// Observe the pinned protocol epoch on a READ COMMITTED transaction that
    /// acquired the shared catalog fence as its first statement. Contention
    /// rolls back before backoff and starts a fresh transaction, so no stale
    /// transaction snapshot waits on the fence. Every successful observation
    /// ends with an acknowledged rollback. The deadline covers pool checkout,
    /// every lock/preflight statement, retries, and rollback. An elapsed
    /// deadline returns an unconfirmed backend identity and closes that pool
    /// session; callers must not treat the client timeout as proof that the
    /// server has already released its locks. Dropping the caller future also
    /// marks any acquired session for close-on-drop.
    pub async fn observe_protocol_epoch(&self) -> Result<ProtocolEpoch, PinnedSelectedEntryError> {
        self.observe_with_deadline(ENTRY_DEADLINE, None, None, None, None)
            .await
    }

    /// Inspect the explicitly limited V1 principal, heap and direct-ACL
    /// profile on the same rollback-only transaction as the existing pinned
    /// physical, V1030 ledger, epoch and manifest checks. No reusable proof or
    /// serving authority escapes. The pool hook runs its separate pinned
    /// attestor before this method can start its own transaction. The provider
    /// must enforce no login trigger and privileged DDL, role, ACL and index
    /// quiescence from before connection until the rollback is acknowledged.
    pub async fn inspect_partial_profile_v1(
        &self,
        profile: &PinnedPartialProfilePinsV1,
    ) -> Result<(), PinnedSelectedEntryError> {
        self.observe_with_deadline(ENTRY_DEADLINE, None, Some(profile), None, None)
            .await
            .map(|_| ())
    }

    /// Inspect the fixed, partial H and Push-family ACL profile on this exact
    /// pinned transaction. This is not custody evidence or serving authority.
    /// The provider must exclude every mutation of the scanned catalogs from
    /// before the pool hook until rollback is acknowledged, including changes
    /// by other sessions that can create temporary schemas. The bounded
    /// preflight and helper checks use separate READ COMMITTED statements.
    pub async fn inspect_push_profile_v1(
        &self,
        profile: &PinnedPushProfilePinsV1,
    ) -> Result<(), PinnedSelectedEntryError> {
        self.observe_with_deadline(
            ENTRY_DEADLINE,
            None,
            Some(&profile.partial),
            None,
            Some(profile),
        )
        .await
        .map(|_| ())
    }

    #[cfg(feature = "test-support")]
    pub async fn inspect_push_profile_v1_paused_for_test(
        &self,
        profile: &PinnedPushProfilePinsV1,
        ready: tokio::sync::oneshot::Sender<i32>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<(), PinnedSelectedEntryError> {
        self.observe_with_deadline(
            ENTRY_DEADLINE,
            Some((ready, release)),
            Some(&profile.partial),
            None,
            Some(profile),
        )
        .await
        .map(|_| ())
    }

    #[cfg(feature = "test-support")]
    pub async fn inspect_partial_profile_v1_paused_for_test(
        &self,
        profile: &PinnedPartialProfilePinsV1,
        ready: tokio::sync::oneshot::Sender<i32>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<(), PinnedSelectedEntryError> {
        self.observe_with_deadline(
            ENTRY_DEADLINE,
            Some((ready, release)),
            Some(profile),
            None,
            None,
        )
        .await
        .map(|_| ())
    }

    #[cfg(feature = "test-support")]
    pub async fn inspect_partial_profile_v1_before_attestor_paused_for_test(
        &self,
        profile: &PinnedPartialProfilePinsV1,
        ready: tokio::sync::oneshot::Sender<i32>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<(), PinnedSelectedEntryError> {
        self.observe_with_deadline(
            ENTRY_DEADLINE,
            None,
            Some(profile),
            Some((ready, release)),
            None,
        )
        .await
        .map(|_| ())
    }

    #[cfg(feature = "test-support")]
    pub async fn observe_protocol_epoch_paused_for_test(
        &self,
        ready: tokio::sync::oneshot::Sender<i32>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<ProtocolEpoch, PinnedSelectedEntryError> {
        self.observe_with_deadline(ENTRY_DEADLINE, Some((ready, release)), None, None, None)
            .await
    }

    #[cfg(feature = "test-support")]
    pub async fn observe_protocol_epoch_with_timeout_paused_for_test(
        &self,
        timeout: Duration,
        ready: tokio::sync::oneshot::Sender<i32>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<ProtocolEpoch, PinnedSelectedEntryError> {
        self.observe_with_deadline(timeout, Some((ready, release)), None, None, None)
            .await
    }

    async fn observe_with_deadline(
        &self,
        timeout: Duration,
        pause: Option<EntryPause>,
        partial_profile: Option<&PinnedPartialProfilePinsV1>,
        before_attestor: Option<EntryPause>,
        push_profile: Option<&PinnedPushProfilePinsV1>,
    ) -> Result<ProtocolEpoch, PinnedSelectedEntryError> {
        let mut backend_pid = None;
        match tokio::time::timeout(
            timeout,
            self.observe_protocol_epoch_inner(
                &mut backend_pid,
                pause,
                partial_profile,
                before_attestor,
                push_profile,
            ),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(PinnedSelectedEntryError::ObservationUnconfirmed { backend_pid }),
        }
    }

    async fn observe_protocol_epoch_inner(
        &self,
        backend_pid: &mut Option<i32>,
        pause: Option<EntryPause>,
        partial_profile: Option<&PinnedPartialProfilePinsV1>,
        mut before_attestor: Option<EntryPause>,
        push_profile: Option<&PinnedPushProfilePinsV1>,
    ) -> Result<ProtocolEpoch, PinnedSelectedEntryError> {
        let AttestationMode::Pinned(authority) = &self.pool.writer_capability.mode else {
            return Err(PinnedSelectedEntryError::NotPinned);
        };
        if partial_profile.is_some_and(|profile| !profile.valid_for(authority)) {
            return Err(PinnedPartialProfileError::InvalidPins.into());
        }
        if push_profile.is_some_and(|profile| !profile.valid_for(authority)) {
            return Err(PinnedPushProfileError::InvalidPins.into());
        }
        let mut connection = DiscardOnDrop::new(self.pool.acquire().await?);
        let pid: i32 = match sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
            .fetch_one(&mut *connection)
            .await
        {
            Ok(pid) => pid,
            Err(_) => {
                return Err(PinnedSelectedEntryError::ObservationUnconfirmed { backend_pid: None });
            }
        };
        *backend_pid = Some(pid);

        loop {
            let mut tx = match connection
                .begin_with("BEGIN ISOLATION LEVEL READ COMMITTED READ ONLY")
                .await
            {
                Ok(tx) => tx,
                Err(_) => {
                    return Err(PinnedSelectedEntryError::ObservationUnconfirmed {
                        backend_pid: Some(pid),
                    });
                }
            };

            // Keep this as the first SQL statement after BEGIN. A false result
            // is not waited on inside the transaction: rollback releases the
            // snapshot before the next bounded try.
            let acquired = sqlx::query_scalar::<_, bool>(
                "SELECT pg_catalog.pg_try_advisory_xact_lock_shared($1)",
            )
            .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
            .fetch_one(&mut *tx)
            .await;
            match acquired {
                Ok(true) => {}
                Ok(false) => {
                    if !rollback_transaction(tx).await {
                        return Err(PinnedSelectedEntryError::ObservationUnconfirmed {
                            backend_pid: Some(pid),
                        });
                    }
                    tokio::time::sleep(RETRY_INTERVAL).await;
                    continue;
                }
                Err(error) => {
                    if !rollback_transaction(tx).await {
                        return Err(PinnedSelectedEntryError::ObservationUnconfirmed {
                            backend_pid: Some(pid),
                        });
                    }
                    return Err(PinnedSelectedEntryError::Postgres(error));
                }
            }

            let epoch = match prepare_selected_transaction(
                &mut tx,
                authority,
                partial_profile,
                before_attestor.take(),
            )
            .await
            {
                Ok(epoch) => epoch,
                Err(error) => {
                    if !rollback_transaction(tx).await {
                        return Err(PinnedSelectedEntryError::ObservationUnconfirmed {
                            backend_pid: Some(pid),
                        });
                    }
                    return Err(error);
                }
            };

            if let Some(profile) = push_profile {
                if let Err(error) =
                    super::pinned_push_profile::inspect(&mut tx, authority, profile).await
                {
                    if !rollback_transaction(tx).await {
                        return Err(PinnedSelectedEntryError::ObservationUnconfirmed {
                            backend_pid: Some(pid),
                        });
                    }
                    return Err(error.into());
                }
            }

            if let Some((ready, release)) = pause {
                if ready.send(pid).is_err() || release.await.is_err() {
                    if !rollback_transaction(tx).await {
                        return Err(PinnedSelectedEntryError::ObservationUnconfirmed {
                            backend_pid: Some(pid),
                        });
                    }
                    return Err(PinnedSelectedEntryError::PauseCancelled);
                }
            }

            if !rollback_transaction(tx).await {
                return Err(PinnedSelectedEntryError::ObservationUnconfirmed {
                    backend_pid: Some(pid),
                });
            }
            connection.keep();
            return Ok(epoch);
        }
    }
}

/// A cancelled or timed-out observation must never return its uncertain
/// backend to the pool. `close_on_drop` is asynchronous; it discards the pool
/// session but is not proof the server already terminated it. Successful,
/// acknowledged rollbacks explicitly keep it.
pub(super) struct DiscardOnDrop {
    connection: PoolConnection<Postgres>,
    keep: bool,
}

impl DiscardOnDrop {
    pub(super) fn new(connection: PoolConnection<Postgres>) -> Self {
        Self {
            connection,
            keep: false,
        }
    }

    pub(super) fn keep(&mut self) {
        self.keep = true;
    }
}

impl Deref for DiscardOnDrop {
    type Target = PgConnection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

impl DerefMut for DiscardOnDrop {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}

impl Drop for DiscardOnDrop {
    fn drop(&mut self) {
        if !self.keep {
            self.connection.close_on_drop();
        }
    }
}

pub(super) async fn rollback_transaction(tx: Transaction<'_, Postgres>) -> bool {
    match tokio::time::timeout(Duration::from_secs(5), tx.rollback()).await {
        Ok(Ok(())) => true,
        Ok(Err(_)) | Err(_) => false,
    }
}

async fn prepare_selected_transaction(
    tx: &mut Transaction<'_, Postgres>,
    authority: &PinnedRuntimeAuthority,
    partial_profile: Option<&PinnedPartialProfilePinsV1>,
    before_attestor: Option<EntryPause>,
) -> Result<ProtocolEpoch, PinnedSelectedEntryError> {
    for statement in [
        "SET LOCAL lock_timeout = '5s'",
        "SET LOCAL statement_timeout = '10s'",
        "SET LOCAL idle_in_transaction_session_timeout = '10s'",
        "SET LOCAL transaction_timeout = '20s'",
    ] {
        sqlx::query(statement).execute(&mut **tx).await?;
    }
    let schema_name = &authority.config.postgres().schema.name.0;
    let restore_path = format!("{}, pg_temp", super::quote_identifier(schema_name));
    if let Some(profile) = partial_profile {
        sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
            .execute(&mut **tx)
            .await?;
        super::verify_pinned_partial_profile(tx, authority, profile).await?;
        if let Some((ready, release)) = before_attestor {
            let pid = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
                .fetch_one(&mut **tx)
                .await?;
            if ready.send(pid).is_err() || release.await.is_err() {
                return Err(PinnedSelectedEntryError::PauseCancelled);
            }
        }
        attest_dark_catalog_pinned_with_partial_profile(tx, authority, profile, &restore_path)
            .await?;
        verify_dark_shared_migration_ledger_v1030_partial_profile_sqlx(
            tx,
            authority,
            profile,
            schema_name,
            authority.ledger,
            &restore_path,
        )
        .await?;
    } else {
        attest_dark_catalog_pinned(tx, &authority.config, authority.attestor, &restore_path)
            .await?;
        verify_dark_shared_migration_ledger_v1030_read_sqlx(
            tx,
            schema_name,
            authority.ledger,
            &restore_path,
        )
        .await?;
    }
    verify_pinned_epoch_manifest_catalog_sqlx(tx, authority).await?;
    if let Some(profile) = partial_profile {
        sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
            .execute(&mut **tx)
            .await?;
        super::verify_pinned_partial_profile(tx, authority, profile).await?;
        sqlx::query("SELECT pg_catalog.set_config('search_path', $1, true)")
            .bind(&restore_path)
            .execute(&mut **tx)
            .await?;
    }
    let connection: &mut PgConnection = tx;
    let epoch = read_epoch_after_prelock(
        connection,
        schema_name,
        authority.config.postgres().schema.oid,
    )
    .await?;
    Ok(epoch)
}
