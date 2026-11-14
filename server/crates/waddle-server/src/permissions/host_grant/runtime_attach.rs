//! Verification of the runtime role's capabilities on the host-grant
//! relations, and of the authority it attaches to.
//!
//! The canonical store relies on what its role cannot do: the runtime role
//! cannot enroll a scope, bind a room or change the authority incarnation
//! (it only reads those relations), and it cannot delete or truncate any
//! host-grant row or rewrite the operation journal. The owner phase grants
//! exactly that set from its grant matrix and refuses a runtime role that
//! already holds more. This check reads the same profile back from the
//! catalog on a runtime handle; later privilege changes by the owner are not
//! observed.
//!
//! A replica runs the check and the restore fence (`restore`) before it
//! admits a managed room, and keeps the result as a [`HostGrantAttachment`]
//! in its [`HostGrantGate`]. An acceptance transaction admits a managed room
//! only on a replica whose gate holds one, so a replica whose runtime role
//! does not match the profile, or whose authority the fence refuses, treats
//! every enrolled scope as unavailable.
//!
//! The attachment binds the replica to the authority as the fence found it
//! ([`AuthorityBinding`]): the binding read at the start of the fence pass
//! and read again at its end, so the attachment never carries a binding the
//! fence did not cover. Every acceptance compares it with what it reads.
//! When an acceptance finds it changed (a restore transition, a restore,
//! move or restart of the database under the running replica), the gate
//! verifies again in place, once at a time: it runs the fence, and swaps the
//! attachment only for one the fence produced. Until then every acceptance
//! for a managed room is refused, and while a verification after a change
//! has failed the replica reports itself not ready.
//!
//! A restore that rewrites the host-grant rows in place, without a restart
//! of PostgreSQL or a new relation, changes nothing a binding holds. The
//! gate therefore also runs the fence periodically while it is attached
//! ([`REVERIFY_INTERVAL`]): evidence about a scope closes it in PostgreSQL,
//! where every replica's acceptances read it, and a refusal of the whole
//! authority drops the attachment until a later pass succeeds.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use jid::BareJid;
use kameo::actor::ActorRef;
use tokio_util::sync::CancellationToken;

use super::error::HostGrantError;
use super::issuance::ServedAccountDomains;
use super::restore::{verify_restore_fence, AuthorityBinding, RestoreEpoch};
use super::store::HostGrantStore;
use super::witness::{room_witness_digest, sync_journal_witness, sync_position_witness};
use crate::db::read_only_timeout::{bound_postgres_waits, ReadOnlyTimeoutError};
use crate::db::{runtime_store_acl, Database, DatabaseDriver, Transaction};
use crate::permissions::PermissionActor;

/// The first delay before a failed verification is repeated; each further
/// failure doubles it up to [`VERIFY_RETRY_MAX`].
const VERIFY_RETRY_INITIAL: Duration = Duration::from_secs(1);
const VERIFY_RETRY_MAX: Duration = Duration::from_secs(60);

/// How often an attached gate runs the restore fence again.
pub const REVERIFY_INTERVAL: Duration = Duration::from_secs(300);

/// What a replica established about its host-grant store: its runtime role
/// holds exactly the store's privileges, the restore fence passed, and the
/// authority it verified. It also carries the account domains the replica
/// serves, which admission honours privileged grants for, and the rooms of
/// lifecycles only SpiceDB knows, which it refuses.
///
/// The value exists only after the checks passed, and an acceptance
/// transaction admits a managed room only when its unit of work holds one.
#[derive(Clone, Debug)]
pub struct HostGrantAttachment {
    served: ServedAccountDomains,
    binding: AuthorityBinding,
    refused_rooms: Arc<BTreeSet<String>>,
}

impl HostGrantAttachment {
    /// Verify the role of `store`'s handle against the runtime profile, run
    /// the restore fence for the replica's `configured` epoch, and bind the
    /// attachment to the authority the fence verified.
    pub async fn verify(
        store: &HostGrantStore,
        permissions: &ActorRef<PermissionActor>,
        served: ServedAccountDomains,
        configured: RestoreEpoch,
    ) -> Result<Self, HostGrantError> {
        store.verify_runtime_privileges().await?;
        Self::fenced(store, permissions, served, configured).await
    }

    /// The restore fence and the binding, without the privilege check, for
    /// suites whose database login owns the schema it tests.
    #[cfg(feature = "test-support")]
    pub async fn fenced_for_test(
        store: &HostGrantStore,
        permissions: &ActorRef<PermissionActor>,
        served: ServedAccountDomains,
        configured: RestoreEpoch,
    ) -> Result<Self, HostGrantError> {
        Self::fenced(store, permissions, served, configured).await
    }

    /// An attachment bound to the authority `store` reads now, without the
    /// privilege check and without the fence, for suites that exercise
    /// admission and own the schema they test.
    #[cfg(feature = "test-support")]
    pub async fn unfenced_for_test(
        store: &HostGrantStore,
        served: ServedAccountDomains,
    ) -> Result<Self, HostGrantError> {
        Ok(Self {
            served,
            binding: store.authority_binding().await?,
            refused_rooms: Arc::default(),
        })
    }

    /// An attachment bound to `binding` as given, for cases that stand in
    /// for an authority that changed after the replica attached.
    #[cfg(feature = "test-support")]
    pub fn with_binding_for_test(served: ServedAccountDomains, binding: AuthorityBinding) -> Self {
        Self {
            served,
            binding,
            refused_rooms: Arc::default(),
        }
    }

    async fn fenced(
        store: &HostGrantStore,
        permissions: &ActorRef<PermissionActor>,
        served: ServedAccountDomains,
        configured: RestoreEpoch,
    ) -> Result<Self, HostGrantError> {
        let report = verify_restore_fence(store, permissions, configured).await?;
        let binding = report.binding.ok_or(HostGrantError::CorruptRow {
            field: "authority incarnation",
        })?;
        if report.awaiting_reconstruction + report.awaiting_reopen + report.fenced > 0
            || report.orphaned > 0
        {
            tracing::warn!(
                awaiting_reconstruction = report.awaiting_reconstruction,
                awaiting_reopen = report.awaiting_reopen,
                fenced = report.fenced,
                orphaned = report.orphaned,
                "Host-grant scopes are closed for a restore, or lifecycles only SpiceDB holds \
                 refuse their rooms (docs/operations/authz-host-grant-restore.md)"
            );
        }
        // The fence passed: bring the witnesses up to date, off the path of
        // any acceptance.
        tokio::spawn(sync_witnesses_after_fence(
            store.clone(),
            permissions.clone(),
        ));
        Ok(Self {
            served,
            binding,
            refused_rooms: Arc::new(report.refused_rooms),
        })
    }

    /// The account domains this replica serves.
    pub fn served(&self) -> &ServedAccountDomains {
        &self.served
    }

    /// The authority this replica verified, which every acceptance compares
    /// with what it reads.
    pub fn binding(&self) -> &AuthorityBinding {
        &self.binding
    }

    /// Whether `room`, which no scope in the canonical store manages, is a
    /// room of a lifecycle only SpiceDB holds: an enrollment the restored
    /// database lost. Such a room is refused rather than served as a room
    /// no scope manages.
    pub fn refuses_unmanaged(&self, room: &BareJid) -> bool {
        if self.refused_rooms.is_empty() {
            return false;
        }
        waddle_xmpp::parse_managed_room_jid(room).is_some_and(|channel_id| {
            self.refused_rooms
                .contains(&room_witness_digest(&channel_id))
        })
    }
}

/// Bring the position witness and every scope's journal witness up to date
/// after a fence passed. Failures are logged by kind only.
async fn sync_witnesses_after_fence(store: HostGrantStore, permissions: ActorRef<PermissionActor>) {
    if let Err(error) = sync_position_witness(&store, &permissions).await {
        tracing::debug!(
            kind = error.kind(),
            "the host-grant position witness was not brought up to date"
        );
    }
    let mut after = None;
    loop {
        let page = match store.scope_page(after).await {
            Ok(page) => page,
            Err(error) => {
                tracing::debug!(
                    kind = error.kind(),
                    "the host-grant journal witnesses were not brought up to date"
                );
                return;
            }
        };
        let Some(last) = page.last().copied() else {
            return;
        };
        for lifecycle in page {
            if let Err(error) = sync_journal_witness(&store, &permissions, lifecycle).await {
                tracing::debug!(
                    kind = error.kind(),
                    "a host-grant journal witness was not brought up to date"
                );
            }
        }
        after = Some(last);
    }
}

/// How a replica verifies its host-grant attachment again.
#[derive(Clone)]
struct GateVerifier {
    store: HostGrantStore,
    permissions: ActorRef<PermissionActor>,
    served: ServedAccountDomains,
    configured: RestoreEpoch,
    /// Whether the runtime role's privileges are checked too; only suites
    /// whose login owns the schema skip it.
    privileges: bool,
    stop: CancellationToken,
}

impl GateVerifier {
    async fn verify(&self) -> Result<HostGrantAttachment, HostGrantError> {
        if self.privileges {
            HostGrantAttachment::verify(
                &self.store,
                &self.permissions,
                self.served.clone(),
                self.configured,
            )
            .await
        } else {
            HostGrantAttachment::fenced(
                &self.store,
                &self.permissions,
                self.served.clone(),
                self.configured,
            )
            .await
        }
    }
}

/// Where a gate stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateState {
    /// No attachment yet: the replica has not verified the store.
    Unattached,
    /// The attachment is the one acceptances compare with.
    Attached,
    /// An acceptance found the authority changed; the gate is verifying it
    /// again and refuses managed rooms meanwhile.
    Reverifying,
    /// Verifying the changed authority failed; the gate keeps trying and
    /// refuses managed rooms meanwhile. The replica is not ready.
    ReverifyFailed,
}

#[derive(Debug)]
struct GateInner {
    attachment: Option<HostGrantAttachment>,
    state: GateState,
}

/// The host-grant attachment of one replica, shared by every acceptance it
/// makes, and the single verification that replaces it when the authority
/// changed under the running replica.
pub struct HostGrantGate {
    inner: RwLock<GateInner>,
    verifier: OnceLock<GateVerifier>,
    verifying: AtomicBool,
    watching: AtomicBool,
}

impl Default for HostGrantGate {
    fn default() -> Self {
        Self {
            inner: RwLock::new(GateInner {
                attachment: None,
                state: GateState::Unattached,
            }),
            verifier: OnceLock::new(),
            verifying: AtomicBool::new(false),
            watching: AtomicBool::new(false),
        }
    }
}

impl std::fmt::Debug for HostGrantGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HostGrantGate")
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl HostGrantGate {
    /// Hold `attachment` if the gate holds none yet. The first attachment
    /// stays until a verification after a change replaces it; the answer is
    /// whether this call set it.
    pub fn attach(&self, attachment: HostGrantAttachment) -> bool {
        let mut inner = self.write();
        if inner.attachment.is_some() {
            return false;
        }
        inner.attachment = Some(attachment);
        inner.state = GateState::Attached;
        true
    }

    /// The attachment acceptances compare with, if any.
    pub fn current(&self) -> Option<HostGrantAttachment> {
        self.read().attachment.clone()
    }

    pub fn state(&self) -> GateState {
        self.read().state
    }

    /// Whether the replica may report itself ready: not while a
    /// verification after a change of the authority has failed.
    pub fn ready(&self) -> bool {
        self.state() != GateState::ReverifyFailed
    }

    /// Install how this gate verifies its attachment: the runtime role's
    /// privileges, then the restore fence. The first installation stays.
    pub fn install_verifier(
        &self,
        store: HostGrantStore,
        permissions: ActorRef<PermissionActor>,
        served: ServedAccountDomains,
        configured: RestoreEpoch,
        stop: CancellationToken,
    ) {
        let _ = self.verifier.set(GateVerifier {
            store,
            permissions,
            served,
            configured,
            privileges: true,
            stop,
        });
    }

    /// Install a verifier that runs the restore fence without the privilege
    /// check, for suites whose login owns the schema.
    #[cfg(feature = "test-support")]
    pub fn install_verifier_for_test(
        &self,
        store: HostGrantStore,
        permissions: ActorRef<PermissionActor>,
        served: ServedAccountDomains,
        configured: RestoreEpoch,
    ) {
        let _ = self.verifier.set(GateVerifier {
            store,
            permissions,
            served,
            configured,
            privileges: false,
            stop: CancellationToken::new(),
        });
    }

    /// Verify the store once and hold the attachment it produces. A gate
    /// without a verifier has nothing to verify.
    pub async fn verify_once(&self) -> Result<(), HostGrantError> {
        let Some(verifier) = self.verifier.get() else {
            return Ok(());
        };
        let attachment = verifier.verify().await?;
        self.attach(attachment);
        Ok(())
    }

    /// Keep verifying in the background until an attempt passes, the
    /// failure is one no later attempt passes, or the replica stops.
    pub fn verify_in_background(self: &Arc<Self>) {
        if self.verifier.get().is_none() || self.verifying.swap(true, Ordering::AcqRel) {
            return;
        }
        let gate = Arc::clone(self);
        tokio::spawn(async move {
            gate.verify_until_attached(false).await;
            gate.verifying.store(false, Ordering::Release);
        });
    }

    /// An acceptance found the authority changed from the attachment's
    /// binding. Unless a verification already runs, start one: it runs the
    /// restore fence, which reads the binding before and after it, and
    /// replaces the attachment with the one the fence produced. Until it
    /// passes every acceptance for a managed room is refused, and once an
    /// attempt failed the replica reports itself not ready.
    pub fn authority_changed(self: &Arc<Self>) {
        if self.verifier.get().is_none() || self.verifying.swap(true, Ordering::AcqRel) {
            return;
        }
        {
            let mut inner = self.write();
            if inner.state == GateState::Attached {
                inner.state = GateState::Reverifying;
            }
        }
        let gate = Arc::clone(self);
        tokio::spawn(async move {
            gate.verify_until_attached(true).await;
            gate.verifying.store(false, Ordering::Release);
        });
    }

    /// Run the restore fence again every `interval` while the gate holds an
    /// attachment, until the replica stops. A pass that succeeds replaces
    /// the attachment; one the fence refuses for the whole authority drops
    /// it, reports the replica not ready and keeps verifying until a pass
    /// succeeds; a failure a later pass may not repeat (SpiceDB or
    /// PostgreSQL not answering) keeps the attachment. Evidence about one
    /// scope closes that scope in PostgreSQL either way. One watch runs per
    /// gate; the production interval is [`REVERIFY_INTERVAL`].
    pub fn watch(self: &Arc<Self>, interval: Duration) {
        if self.verifier.get().is_none() || self.watching.swap(true, Ordering::AcqRel) {
            return;
        }
        let gate = Arc::clone(self);
        tokio::spawn(async move {
            let Some(verifier) = gate.verifier.get() else {
                return;
            };
            loop {
                tokio::select! {
                    () = verifier.stop.cancelled() => return,
                    () = tokio::time::sleep(interval) => {}
                }
                if gate.state() != GateState::Attached
                    || gate.verifying.swap(true, Ordering::AcqRel)
                {
                    continue;
                }
                match verifier.verify().await {
                    Ok(attachment) => {
                        let mut inner = gate.write();
                        inner.attachment = Some(attachment);
                        inner.state = GateState::Attached;
                    }
                    Err(HostGrantError::RestoreFence(refusal))
                        if refusal != super::restore::RestoreFenceRefusal::AuthorityMoved =>
                    {
                        let error = HostGrantError::RestoreFence(refusal);
                        log_unverified(&verifier.store, &error, true).await;
                        {
                            let mut inner = gate.write();
                            inner.attachment = None;
                            inner.state = GateState::ReverifyFailed;
                        }
                        gate.verify_until_attached(true).await;
                    }
                    Err(error) => {
                        tracing::debug!(
                            kind = error.kind(),
                            "the periodic host-grant restore fence did not complete"
                        );
                    }
                }
                gate.verifying.store(false, Ordering::Release);
            }
        });
    }

    /// Wait until a running verification, if any, has finished. For suites
    /// that observe the gate after a change.
    #[cfg(feature = "test-support")]
    pub async fn settled(&self) {
        while self.verifying.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn verify_until_attached(&self, after_change: bool) {
        let Some(verifier) = self.verifier.get() else {
            return;
        };
        let mut delay = VERIFY_RETRY_INITIAL;
        loop {
            match verifier.verify().await {
                Ok(attachment) => {
                    let mut inner = self.write();
                    inner.attachment = Some(attachment);
                    inner.state = GateState::Attached;
                    return;
                }
                Err(error) => {
                    let retried = verification_retried(&error);
                    log_unverified(&verifier.store, &error, retried).await;
                    if after_change {
                        let mut inner = self.write();
                        // The attachment no longer matches the authority; an
                        // acceptance finds that out, but readiness is told.
                        inner.state = GateState::ReverifyFailed;
                    }
                    if !retried {
                        return;
                    }
                }
            }
            tokio::select! {
                () = verifier.stop.cancelled() => return,
                () = tokio::time::sleep(delay) => {}
            }
            delay = delay.saturating_mul(2).min(VERIFY_RETRY_MAX);
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, GateInner> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, GateInner> {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Whether a later verification may pass after `error`. A runtime role that
/// does not match the profile, or a store that cannot be read as the
/// profile requires, does not change by itself; everything else, a refusal
/// of the restore fence included, may change with an operator's act (a
/// declared restore, an acknowledged position, a corrected configuration)
/// and is tried again.
pub fn verification_retried(error: &HostGrantError) -> bool {
    !matches!(
        error,
        HostGrantError::PostgresRequired
            | HostGrantError::UnsafeRuntimePrivileges { .. }
            | HostGrantError::UnexpectedExecutableSchema { .. }
            | HostGrantError::InvalidTimeoutSetting { .. }
    )
}

/// Log a failed verification by its kind only. The failure is an error
/// when an unretired scope is enrolled, which this replica cannot serve, or
/// when that cannot be read; otherwise the replica loses nothing by it and
/// it is logged as information.
pub async fn log_unverified(store: &HostGrantStore, error: &HostGrantError, retrying: bool) {
    const UNVERIFIED: &str = "The runtime role's privileges on the host-grant store, or its \
                              restore fence, are not verified; rooms of enrolled scopes are \
                              unavailable on this replica";
    let refusal = match error {
        HostGrantError::RestoreFence(refusal) => refusal.kind(),
        _ => "none",
    };
    match store.holds_unretired_scope().await {
        Ok(false) => tracing::info!(kind = error.kind(), refusal, retrying, "{UNVERIFIED}"),
        Ok(true) => tracing::error!(
            kind = error.kind(),
            refusal,
            retrying,
            unretired_scope = true,
            "{UNVERIFIED}"
        ),
        Err(probe) => tracing::error!(
            kind = error.kind(),
            refusal,
            retrying,
            probe_kind = probe.kind(),
            "{UNVERIFIED}"
        ),
    }
}

/// Every host-grant relation, locked before the catalog is read.
const RELATIONS: [&str; 7] = [
    "host_grant_authority",
    "host_grant_scope_lifecycles",
    "host_grant_scopes",
    "host_grant_rooms",
    "host_grants",
    "host_grant_operations",
    "host_grant_operation_changes",
];

fn quoted(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

async fn begin_bounded_snapshot(db: &Database) -> Result<Transaction<'_>, HostGrantError> {
    let mut tx = db.begin().await?;
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
        (),
    )
    .await?;
    bound_postgres_waits(&mut tx)
        .await
        .map_err(|error| match error {
            ReadOnlyTimeoutError::InvalidSetting { setting } => {
                HostGrantError::InvalidTimeoutSetting { setting }
            }
            ReadOnlyTimeoutError::Database(error) => HostGrantError::Database(error),
        })?;
    Ok(tx)
}

/// Check the role and its privileges on the locked relations.
async fn verify_relations(tx: &mut Transaction<'_>, schema: &str) -> Result<(), HostGrantError> {
    if let Some(component) =
        runtime_store_acl::unexpected_host_grant_executable_schema(tx, schema).await?
    {
        return Err(HostGrantError::UnexpectedExecutableSchema { component });
    }
    if let Some(component) = runtime_store_acl::verify_host_grants(tx, schema).await? {
        return Err(HostGrantError::UnsafeRuntimePrivileges { component });
    }
    Ok(())
}

/// Lock every relation before the first snapshot-forming statement, so the
/// owner cannot commit a change between the snapshot and the lock. Without a
/// schema the names resolve through the handle's search path.
async fn lock_relations(
    tx: &mut Transaction<'_>,
    schema: Option<&str>,
) -> Result<(), HostGrantError> {
    for relation in RELATIONS {
        let name = match schema {
            Some(schema) => format!("{}.{}", quoted(schema), quoted(relation)),
            None => quoted(relation),
        };
        tx.execute(&format!("LOCK TABLE {name} IN ACCESS SHARE MODE"), ())
            .await?;
    }
    Ok(())
}

/// Verify the handle's role against the runtime profile, in the schema its
/// statements resolve to.
pub(super) async fn verify_current_role(db: &Database) -> Result<(), HostGrantError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(HostGrantError::PostgresRequired);
    }
    let mut tx = begin_bounded_snapshot(db).await?;
    lock_relations(&mut tx, None).await?;
    let mut rows = tx
        .query(
            "SELECT n.nspname::text FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.oid = pg_catalog.to_regclass('host_grant_scopes')",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(HostGrantError::UnsafeRuntimePrivileges {
            component: "host_grant_scopes",
        });
    };
    let schema: String = row.get(0)?;
    drop(rows);
    verify_relations(&mut tx, &schema).await?;
    tx.rollback().await?;
    Ok(())
}
