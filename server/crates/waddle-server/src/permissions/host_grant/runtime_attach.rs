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
//! for a managed room is refused.
//!
//! A restore that rewrites the host-grant rows in place, without a restart
//! of PostgreSQL or a new relation, changes nothing a binding holds. The
//! gate therefore also runs the fence periodically while it is attached
//! ([`REVERIFY_INTERVAL`], with jitter): evidence about a scope closes it in
//! PostgreSQL, where every replica's acceptances read it; a refusal of the
//! whole authority, or a runtime role that no longer matches the profile,
//! drops the attachment; and an attachment no pass has confirmed for
//! [`MAX_INCOMPLETE_PASSES`] passes in a row is dropped as well.
//!
//! The gate also keeps what the fence found independently of the
//! attachment ([`FenceMemory`]): the rooms of lifecycles only SpiceDB holds
//! (an enrollment the restored database lost), which are refused whether or
//! not the replica is attached, and a position regression it latched.
//!
//! Readiness. A refusal of the whole authority, a failover the position
//! witness cannot verify included, keeps the replica ready: every replica
//! sees the same authority, so failing readiness would take every replica
//! out of the endpoint set and the rooms no scope manages with them, while
//! managed rooms are refused either way. The failure is an error log at
//! most once a minute and the `waddle.host_grant.authority.unverified`
//! gauge. Only a replica whose configured restore epoch is below the one
//! the database records reports itself not ready: its configuration
//! predates a declared restore, and a restart with the current
//! configuration is what clears it.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use jid::BareJid;
use kameo::actor::ActorRef;
use tokio_util::sync::CancellationToken;

use super::error::HostGrantError;
use super::issuance::ServedAccountDomains;
use super::model::ScopeLifecycleId;
use super::restore::{
    refresh_refused_rooms, verify_restore_fence_with, AuthorityBinding, FenceMemory, RestoreEpoch,
    RestoreFenceRefusal, RestoreFenceReport,
};
use super::store::HostGrantStore;
use super::witness::{
    rate_limited, read_witness, room_witness_digest, room_witness_resource,
    sync_witnesses_after_fence, PositionLatch,
};
use crate::db::read_only_timeout::{bound_postgres_waits, ReadOnlyTimeoutError};
use crate::db::{runtime_store_acl, Database, DatabaseDriver, Transaction};
use crate::permissions::PermissionActor;

/// The first delay before a failed verification is repeated; each further
/// failure doubles it up to [`VERIFY_RETRY_MAX`].
const VERIFY_RETRY_INITIAL: Duration = Duration::from_secs(1);
const VERIFY_RETRY_MAX: Duration = Duration::from_secs(60);

/// How often an attached gate runs the restore fence again, before a jitter
/// of up to a tenth of it.
pub const REVERIFY_INTERVAL: Duration = Duration::from_secs(300);

/// Periodic passes in a row that may fail to complete (PostgreSQL or
/// SpiceDB not answering) before the attachment they could not confirm is
/// dropped.
pub const MAX_INCOMPLETE_PASSES: u32 = 3;

/// Longest an acceptance waits for SpiceDB to say whether a room no scope
/// manages belongs to a lost enrollment, while no fence pass has read the
/// refused rooms.
const ROOM_WITNESS_READ_TIMEOUT: Duration = Duration::from_millis(500);

/// Most answers of that read the gate remembers.
const ROOM_WITNESS_CACHE: usize = 4096;

/// Least time between two error logs about an unverified authority.
const ALERT_INTERVAL_MS: i64 = 60_000;

/// What a replica established about its host-grant store: its runtime role
/// holds exactly the store's privileges, the restore fence passed, and the
/// authority it verified. It also carries the account domains the replica
/// serves, which admission honours privileged grants for.
///
/// The value exists only after the checks passed, and an acceptance
/// transaction admits a managed room only when its unit of work holds one.
#[derive(Clone, Debug)]
pub struct HostGrantAttachment {
    served: ServedAccountDomains,
    binding: AuthorityBinding,
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
        let memory = FenceMemory::default();
        Self::fenced(store, permissions, served, configured, &memory)
            .await
            .map(|(attachment, _)| attachment)
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
        let memory = FenceMemory::default();
        Self::fenced(store, permissions, served, configured, &memory)
            .await
            .map(|(attachment, _)| attachment)
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
        })
    }

    /// An attachment bound to `binding` as given, for cases that stand in
    /// for an authority that changed after the replica attached.
    #[cfg(feature = "test-support")]
    pub fn with_binding_for_test(served: ServedAccountDomains, binding: AuthorityBinding) -> Self {
        Self { served, binding }
    }

    async fn fenced(
        store: &HostGrantStore,
        permissions: &ActorRef<PermissionActor>,
        served: ServedAccountDomains,
        configured: RestoreEpoch,
        memory: &FenceMemory,
    ) -> Result<(Self, RestoreFenceReport), HostGrantError> {
        let report = verify_restore_fence_with(store, permissions, configured, memory).await?;
        let binding = report.binding.clone().ok_or(HostGrantError::CorruptRow {
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
        Ok((Self { served, binding }, report))
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
    async fn verify(
        &self,
        memory: &FenceMemory,
    ) -> Result<(HostGrantAttachment, RestoreFenceReport), HostGrantError> {
        if self.privileges {
            if let Err(error) = self.store.verify_runtime_privileges().await {
                // The rooms of lost enrollments are refused whether or not
                // this replica can attach.
                if let Err(rooms) =
                    refresh_refused_rooms(&self.store, &self.permissions, memory).await
                {
                    tracing::debug!(
                        kind = rooms.kind(),
                        "the rooms of lifecycles only SpiceDB holds were not read"
                    );
                }
                return Err(error);
            }
        }
        HostGrantAttachment::fenced(
            &self.store,
            &self.permissions,
            self.served.clone(),
            self.configured,
            memory,
        )
        .await
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
    /// Verifying the authority again failed, after an acceptance found it
    /// changed or on a periodic pass; the gate holds no attachment it could
    /// confirm, refuses managed rooms and keeps trying.
    ReverifyFailed,
}

#[derive(Debug)]
struct GateInner {
    attachment: Option<HostGrantAttachment>,
    state: GateState,
    /// The last verification failed because the configured restore epoch is
    /// below the one the database records.
    stale_configuration: bool,
}

/// The host-grant attachment of one replica, shared by every acceptance it
/// makes, the single verification that replaces it when the authority
/// changed under the running replica, and what the fence found that holds
/// whether or not the replica is attached.
pub struct HostGrantGate {
    inner: RwLock<GateInner>,
    verifier: OnceLock<GateVerifier>,
    memory: FenceMemory,
    verifying: AtomicBool,
    watching: AtomicBool,
    syncing: Arc<AtomicBool>,
    room_reads: Mutex<HashMap<String, bool>>,
    last_alert_ms: AtomicI64,
}

impl Default for HostGrantGate {
    fn default() -> Self {
        Self {
            inner: RwLock::new(GateInner {
                attachment: None,
                state: GateState::Unattached,
                stale_configuration: false,
            }),
            verifier: OnceLock::new(),
            memory: FenceMemory::default(),
            verifying: AtomicBool::new(false),
            watching: AtomicBool::new(false),
            syncing: Arc::new(AtomicBool::new(false)),
            room_reads: Mutex::new(HashMap::new()),
            last_alert_ms: AtomicI64::new(i64::MIN),
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
    /// stays until a verification replaces or drops it; the answer is
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

    /// Whether the replica may report itself ready: not while its
    /// configured restore epoch is below the one the database records.
    /// Every other failure to verify the authority keeps it ready (see the
    /// module documentation).
    pub fn ready(&self) -> bool {
        !self.read().stale_configuration
    }

    /// Whether the replica holds an attachment the fence confirmed.
    pub fn verified(&self) -> bool {
        self.state() == GateState::Attached
    }

    /// What the fence found that holds whether or not the replica is
    /// attached.
    pub fn memory(&self) -> &FenceMemory {
        &self.memory
    }

    /// Whether `room`, which no scope in the canonical store manages, is a
    /// room of a lifecycle only SpiceDB holds: an enrollment the restored
    /// database lost. Such a room is refused rather than served as a room
    /// no scope manages.
    ///
    /// The answer comes from the rooms the last fence pass read. Until a
    /// pass has read them, the room's witness is read from SpiceDB, within
    /// [`ROOM_WITNESS_READ_TIMEOUT`], and the answer remembered; when
    /// SpiceDB does not answer the room is served.
    pub async fn refuses_unmanaged(&self, room: &BareJid) -> bool {
        let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room) else {
            return false;
        };
        let digest = room_witness_digest(&channel_id);
        if let Some(rooms) = self.memory.refused_rooms() {
            return rooms.contains(&digest);
        }
        let Some(verifier) = self.verifier.get() else {
            return false;
        };
        if let Some(known) = self.room_reads().get(&digest) {
            return *known;
        }
        let read = tokio::time::timeout(
            ROOM_WITNESS_READ_TIMEOUT,
            read_witness(&verifier.permissions, &room_witness_resource(&channel_id)),
        )
        .await;
        let refused = match read {
            Ok(Ok(Some(listed))) => !listed.stamps.is_empty(),
            Ok(Ok(None)) => false,
            Ok(Err(_)) | Err(_) => return false,
        };
        let mut reads = self.room_reads();
        if reads.len() >= ROOM_WITNESS_CACHE {
            reads.clear();
        }
        reads.insert(digest, refused);
        refused
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
    /// without a verifier has nothing to verify. A failure is logged.
    pub async fn verify_once(&self) -> Result<(), HostGrantError> {
        let Some(verifier) = self.verifier.get() else {
            return Ok(());
        };
        match verifier.verify(&self.memory).await {
            Ok((attachment, report)) => {
                self.attach(attachment);
                self.passed(verifier, &report);
                Ok(())
            }
            Err(error) => {
                self.failed(verifier, &error, verification_retried(&error))
                    .await;
                Err(error)
            }
        }
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
    /// passes every acceptance for a managed room is refused.
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

    /// Run the restore fence again about every `interval` (plus a jitter of
    /// up to a tenth of it) while the gate holds an attachment, until the
    /// replica stops. A pass that succeeds replaces the attachment. One the
    /// fence refuses for the whole authority drops it and keeps verifying
    /// until a pass succeeds; a runtime role that no longer matches the
    /// profile drops it for good. A pass that cannot complete (SpiceDB or
    /// PostgreSQL not answering) keeps the attachment, up to
    /// [`MAX_INCOMPLETE_PASSES`] in a row; then it is dropped and the gate
    /// keeps verifying. Evidence about one scope closes that scope in
    /// PostgreSQL either way. One watch runs per gate; the production
    /// interval is [`REVERIFY_INTERVAL`].
    pub fn watch(self: &Arc<Self>, interval: Duration) {
        if self.verifier.get().is_none() || self.watching.swap(true, Ordering::AcqRel) {
            return;
        }
        let gate = Arc::clone(self);
        tokio::spawn(async move {
            let Some(verifier) = gate.verifier.get() else {
                return;
            };
            let mut incomplete = 0u32;
            loop {
                tokio::select! {
                    () = verifier.stop.cancelled() => return,
                    () = tokio::time::sleep(with_jitter(interval)) => {}
                }
                if gate.state() != GateState::Attached
                    || gate.verifying.swap(true, Ordering::AcqRel)
                {
                    continue;
                }
                match verifier.verify(&gate.memory).await {
                    Ok((attachment, report)) => {
                        incomplete = 0;
                        gate.replace(attachment);
                        gate.passed(verifier, &report);
                    }
                    Err(HostGrantError::RestoreFence(refusal))
                        if refusal != RestoreFenceRefusal::AuthorityMoved =>
                    {
                        let error = HostGrantError::RestoreFence(refusal);
                        gate.drop_attachment();
                        gate.failed(verifier, &error, true).await;
                        gate.verify_until_attached(true).await;
                    }
                    Err(error) if !verification_retried(&error) => {
                        gate.drop_attachment();
                        gate.failed(verifier, &error, false).await;
                    }
                    Err(error) => {
                        incomplete += 1;
                        tracing::debug!(
                            kind = error.kind(),
                            incomplete,
                            "the periodic host-grant restore fence did not complete"
                        );
                        if incomplete >= MAX_INCOMPLETE_PASSES {
                            incomplete = 0;
                            gate.drop_attachment();
                            gate.failed(verifier, &error, true).await;
                            gate.verify_until_attached(true).await;
                        }
                    }
                }
                gate.verifying.store(false, Ordering::Release);
            }
        });
    }

    /// Wait until a running verification, and the witness pass a passed
    /// one starts, have finished. For suites that observe the gate after a
    /// change.
    #[cfg(feature = "test-support")]
    pub async fn settled(&self) {
        while self.verifying.load(Ordering::Acquire) || self.syncing.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn verify_until_attached(&self, after_change: bool) {
        let Some(verifier) = self.verifier.get() else {
            return;
        };
        let mut delay = VERIFY_RETRY_INITIAL;
        loop {
            match verifier.verify(&self.memory).await {
                Ok((attachment, report)) => {
                    self.replace(attachment);
                    self.passed(verifier, &report);
                    return;
                }
                Err(error) => {
                    let retried = verification_retried(&error);
                    if after_change {
                        // The attachment no longer matches the authority; an
                        // acceptance finds that out, and so does the alert.
                        self.write().state = GateState::ReverifyFailed;
                    }
                    self.failed(verifier, &error, retried).await;
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

    fn replace(&self, attachment: HostGrantAttachment) {
        let mut inner = self.write();
        inner.attachment = Some(attachment);
        inner.state = GateState::Attached;
    }

    fn drop_attachment(&self) {
        let mut inner = self.write();
        inner.attachment = None;
        inner.state = GateState::ReverifyFailed;
    }

    /// A verification passed: the configuration is current, the alert
    /// clears, and the witnesses are brought up to date off the path of any
    /// acceptance, one pass at a time.
    fn passed(&self, verifier: &GateVerifier, report: &RestoreFenceReport) {
        self.write().stale_configuration = false;
        record_unverified(false);
        if !report.projection_listed || self.syncing.swap(true, Ordering::AcqRel) {
            return;
        }
        let syncing = Arc::clone(&self.syncing);
        let store = verifier.store.clone();
        let permissions = verifier.permissions.clone();
        let witnessed: BTreeMap<ScopeLifecycleId, String> = report.witnessed_heads.clone();
        let latched: Option<PositionLatch> = self.memory.latch();
        tokio::spawn(async move {
            sync_witnesses_after_fence(&store, &permissions, &witnessed, latched.as_ref()).await;
            syncing.store(false, Ordering::Release);
        });
    }

    /// A verification failed: remember whether the configuration is stale,
    /// and raise the alert by kind only, at most once a minute.
    async fn failed(&self, verifier: &GateVerifier, error: &HostGrantError, retrying: bool) {
        // Only the fence's verdict on the configuration changes the flag; a
        // failure that reached no verdict leaves it as it was.
        if let HostGrantError::RestoreFence(refusal) = error {
            self.write().stale_configuration = matches!(
                refusal,
                RestoreFenceRefusal::EpochMismatch { configured, stored } if configured < stored
            );
        }
        let unserved = verifier.store.holds_unretired_scope().await;
        record_unverified(!matches!(unserved, Ok(false)));
        if rate_limited(&self.last_alert_ms, ALERT_INTERVAL_MS) {
            log_unverified(error, retrying, unserved);
        }
    }

    fn room_reads(&self) -> std::sync::MutexGuard<'_, HashMap<String, bool>> {
        self.room_reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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

/// `interval` plus a random part of up to a tenth of it, so that the
/// replicas of a deployment do not pass their fences at the same moment.
fn with_jitter(interval: Duration) -> Duration {
    let tenth = u64::try_from(interval.as_millis() / 10).unwrap_or(u64::MAX);
    let jitter = if tenth == 0 {
        0
    } else {
        rand::random::<u64>() % tenth
    };
    interval.saturating_add(Duration::from_millis(jitter))
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

/// Set the gauge that alerts on a replica that cannot serve an enrolled
/// scope: 1 while its authority is unverified and an unretired scope is
/// enrolled (or that cannot be read), 0 otherwise.
fn record_unverified(unserved: bool) {
    waddle_xmpp::gauge_record!(
        "waddle.host_grant.authority.unverified",
        "1",
        "1 while this replica holds no verified host-grant attachment and an unretired scope is \
         enrolled: rooms of enrolled scopes are refused here -- alert on 1",
        u64::from(unserved),
    );
}

/// Log a failed verification by its kind only. The failure is an error
/// when an unretired scope is enrolled, which this replica cannot serve, or
/// when that cannot be read; otherwise the replica loses nothing by it and
/// it is logged as information.
fn log_unverified(error: &HostGrantError, retrying: bool, unserved: Result<bool, HostGrantError>) {
    const UNVERIFIED: &str = "The runtime role's privileges on the host-grant store, or its \
                              restore fence, are not verified; rooms of enrolled scopes are \
                              unavailable on this replica";
    let refusal = match error {
        HostGrantError::RestoreFence(refusal) => refusal.kind(),
        _ => "none",
    };
    match unserved {
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
