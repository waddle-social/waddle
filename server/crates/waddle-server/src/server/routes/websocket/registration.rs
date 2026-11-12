use super::connection::{
    BindingConfirmation, BindingPublication, BindingPublisher, BindingRetirement, BindingStage,
};
use super::session_init::load_blocklist_for_bind;
use super::state::WsConnState;
use super::stream_management::{
    finalize_sm_after_registry_registration, reject_unregistered_resume, sm_show_name,
    SmRegistrationFinalization,
};
use super::{error, info, Blocklist, FullJid, WebSocketState};
use crate::account_coordinator::RetireEvidence;
#[cfg(feature = "clustering")]
use crate::clustering::route_bridge::RemoteResourceRegisterOutcome;
use crate::clustering::NodeAdmissionPermit;
use crate::db::DatabaseError;
use crate::server::dual_registration::MirrorRegisterOutcome;
use crate::sm_promotion::DisplacedPromotionDeps;
#[cfg(test)]
use std::sync::Arc;
use waddle_xmpp::registry::{
    DeferredPendingFlushWakeups, SocketOutboundEndpoint, SocketOutboundReceiver,
};

/// A resume can briefly observe the previous node's `UserActor` while its
/// force-detach cleanup drains.  Bound retries keep that handoff from turning
/// an otherwise-valid XEP-0198 resume into a terminal session-init failure.
/// The same budget covers local registration after a foreign claim disappears.
const RESUME_REGISTRATION_BUSY_ATTEMPTS: usize = 3;

const BIND_RETIREMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(test)]
tokio::task_local! {
    pub(super) static PREPARED_BIND_GATE: (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>);
    pub(super) static LOCAL_PUBLICATION_GATE: (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>);
    pub(super) static FOREIGN_OWNER_GATE: (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>);
}

#[derive(Debug, thiserror::Error)]
enum BindPreparationError {
    #[error("resumed occupancy generation is no longer authoritative")]
    ResumeAuthorityRevoked,
    #[error("occupancy authority storage failed: {0}")]
    Authority(#[from] DatabaseError),
    #[error("SM generation inventory failed: {0}")]
    Inventory(#[from] waddle_xmpp::stream_management::SmRegistryError),
    #[error("incumbent occupancy retirement could not be confirmed")]
    RetirementUnconfirmed,
}

async fn prepare_socket_bind(
    state: &WebSocketState,
    jid: &FullJid,
    generation: waddle_xmpp_core::OccupancySessionGeneration,
    resumed: bool,
) -> Result<waddle_xmpp::registry::ConnectionBindGuard, BindPreparationError> {
    use waddle_xmpp::registry::{ForceDetachOrigin, ForceDetachRequest, SocketCleanupState};
    let registry = &state.deps.protocol.connection_registry;
    let mut guard = registry.lock_bind(jid).await;
    let db = state.deps.app_state.db_pool.global();
    if resumed {
        if !crate::occupancy_authority::is_current(db, jid, generation).await? {
            return Err(BindPreparationError::ResumeAuthorityRevoked);
        }
    } else {
        // Snapshot before publication: scanning afterwards could capture and
        // invalidate a newer cross-node bind. Legacy snapshots have no prior
        // authority row, so publication alone cannot discover their lineage.
        let detached_generations = state
            .deps
            .protocol
            .sm_session_registry
            .occupancy_generations_for_full_jid(jid)?;
        // Revoke stale asynchronous room joins before awaiting their cleanup.
        if let Some(displaced) = crate::occupancy_authority::publish(db, jid, generation).await? {
            guard.retain_retirement(displaced);
        }
        for detached in detached_generations {
            if detached != generation {
                guard.retain_retirement(detached);
            }
        }
    }
    if let Some(incumbent) = guard.incumbent() {
        if resumed && incumbent.generation != generation {
            return Err(BindPreparationError::ResumeAuthorityRevoked);
        }
        if incumbent.state() == SocketCleanupState::Running {
            let (ack, _ack_rx) = tokio::sync::oneshot::channel();
            let request = ForceDetachRequest {
                origin: if resumed {
                    ForceDetachOrigin::CrossNodeResume
                } else {
                    ForceDetachOrigin::FreshBindReplacement
                },
                target_binding: incumbent.entry.binding_ref().cloned(),
                requester_bare_jid: jid.to_bare(),
                ack,
            };
            let _ = if resumed {
                incumbent.entry.try_force_detach_and_revoke(request).is_ok()
            } else {
                incumbent
                    .entry
                    .try_force_detach_and_interrupt(request)
                    .is_ok()
            };
            // Neither a closed channel nor a missing route proves cleanup.
            incumbent.wait_stopped().await;
        }
        if !resumed {
            guard.retain_retirement(incumbent.generation);
        }
    }
    if !resumed {
        // The resource is bound again: its durable stream-management sessions
        // end, and what they retained moves to offline delivery. The
        // incumbent has stopped, so its own teardown has run first.
        super::cleanup::replace_durable_resource_sessions(state, jid).await;
    }
    #[cfg(feature = "clustering")]
    if !resumed {
        if let Some(bridge) = state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
        {
            if !bridge
                .retire_remote_incumbent_before_local_bind(jid, generation)
                .await
            {
                return Err(BindPreparationError::RetirementUnconfirmed);
            }
        }
    }
    if !complete_bind_retirements(state, jid, generation, &mut guard, resumed).await {
        return Err(BindPreparationError::RetirementUnconfirmed);
    }
    if !crate::occupancy_authority::is_current(db, jid, generation).await? {
        return Err(if resumed {
            BindPreparationError::ResumeAuthorityRevoked
        } else {
            BindPreparationError::RetirementUnconfirmed
        });
    }
    // A remote-resource mirror may occupy this route without a local socket
    // lifecycle. Its owner registration is separately generation-fenced.
    Ok(guard)
}

/// Drain exact-generation obligations with the bind gate held. Callers retain
/// obligations on timeout/cancellation, so later admission must finish them.
pub(crate) async fn complete_bind_retirements(
    state: &WebSocketState,
    jid: &FullJid,
    generation: waddle_xmpp_core::OccupancySessionGeneration,
    guard: &mut waddle_xmpp::registry::ConnectionBindGuard,
    resumed: bool,
) -> bool {
    let registry = &state.deps.protocol.connection_registry;
    if !resumed {
        for displaced in guard.pending_retirements() {
            let removed = state
                .deps
                .protocol
                .sm_session_registry
                .invalidate_sessions_for_generation(jid, displaced)
                .await;
            let Ok(removed) = removed else {
                return false;
            };
            // Enter promotion's cancellation-safe custody guard before any
            // other await. Never accumulate handed-off batches across another
            // generation's storage operation.
            crate::sm_promotion::promote_displaced_sessions(
                removed.clone(),
                DisplacedPromotionDeps {
                    sm_registry: &state.deps.protocol.sm_session_registry,
                    connection_registry: registry,
                    user_registry: &state.deps.protocol.user_registry,
                    pending_storage: &state.deps.protocol.pending_delivery_storage,
                    blocking_storage: state.deps.protocol.blocking_storage.as_ref(),
                    server_domain: state.deps.auth_state.xmpp_domain.as_str(),
                },
            )
            .await;
            for detached in removed {
                super::cleanup::cleanup_invalidated_detached_session(state, detached, None).await;
            }
        }
    }
    for pending in guard.pending_retirements() {
        if resumed && pending == generation {
            return false;
        }
        if !super::cleanup::retire_occupancy_before_bind(state, jid, pending).await {
            return false;
        }
        guard.complete_retirement(pending);
    }
    true
}

fn resume_registration_busy_backoff(attempt: usize) -> std::time::Duration {
    std::time::Duration::from_millis(50 * (attempt as u64 + 1))
}

pub(super) async fn retry_resumed_registration_busy<F, Fut>(
    mut register: F,
) -> MirrorRegisterOutcome
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = MirrorRegisterOutcome>,
{
    for attempt in 0..RESUME_REGISTRATION_BUSY_ATTEMPTS {
        let outcome = register().await;
        if outcome == MirrorRegisterOutcome::Busy && attempt + 1 < RESUME_REGISTRATION_BUSY_ATTEMPTS
        {
            tokio::time::sleep(resume_registration_busy_backoff(attempt)).await;
            continue;
        }
        return outcome;
    }
    unreachable!("resume registration busy retry loop always returns")
}

pub(super) enum RegistrationAfterFrame {
    Unchanged,
    Registered(SmRegistrationFinalization),
    SessionInitializationFailed,
    /// The resource is still converging on an authoritative actor owner.
    /// The client can establish a new stream and retry registration.
    RegistrationTemporarilyUnavailable,
    AuthorityRevoked,
    /// Resume finalization has moved the durable detached snapshot into the
    /// live connection. Retain registry ownership so normal shutdown cleanup
    /// can store that exact live SM state before unregistering.
    AuthorityRevokedAfterSmFinalization,
}

fn registration_authoritative(
    permit: &NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
) -> bool {
    !shutdown.is_cancelled() && permit.revalidate().is_ok()
}

/// A binding published into the connection registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RegistryPublication {
    /// The registry's ownership token for the published entry.
    pub(super) owner: waddle_xmpp::registry::LocalConnectionBindingId,
    /// Whether the binding continues a resumed session. A resumed binding
    /// can briefly meet the previous node's `UserActor` while its
    /// force-detach cleanup drains, so its confirmation retries a busy
    /// owner.
    pub(super) resumed: bool,
}

/// Why the registry publisher refused a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(super) enum RegistryRefusal {
    #[error("no outbound endpoint was staged for the binding")]
    NoEndpoint,
    #[error("the staged endpoint belongs to a different binding")]
    EndpointBindingMismatch,
    #[error("the connection registry did not accept the entry")]
    NotRegistered,
    #[error("the account's owner did not register the resource: {0:?}")]
    Mirror(MirrorRegisterOutcome),
    #[error("the account's owner did not accept the unregister: {0:?}")]
    MirrorUnregister(crate::server::dual_registration::MirrorUnregisterOutcome),
}

/// What retiring a binding at the remote node that owns its account
/// reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RemoteBindingRetirement {
    /// This build has no cluster relay, so no remote owner holds a proxy.
    #[cfg(not(feature = "clustering"))]
    NoRemoteOwner,
    #[cfg(feature = "clustering")]
    Relay(crate::clustering::route_bridge::RemoteResourceUnregisterOutcome),
}

/// Publishes bindings into the connection registry, mirrors them into the
/// account's `UserActor` (or the remote node's proxy for it), and retires
/// them owner-gated.
pub(super) struct RegistryBindingPublisher<'a> {
    state: &'a WebSocketState,
    // The entry `publish` registers. The registry holds a binding together
    // with its outbound endpoint, which the trait does not carry, so the
    // connection stages it here before it publishes.
    endpoint: std::sync::Mutex<Option<waddle_xmpp::registry::ConnectionEntry>>,
}

impl<'a> RegistryBindingPublisher<'a> {
    pub(super) fn new(state: &'a WebSocketState) -> Self {
        Self {
            state,
            endpoint: std::sync::Mutex::new(None),
        }
    }

    /// A publisher whose next `publish` registers `entry`.
    pub(super) fn with_endpoint(
        state: &'a WebSocketState,
        entry: waddle_xmpp::registry::ConnectionEntry,
    ) -> Self {
        Self {
            state,
            endpoint: std::sync::Mutex::new(Some(entry)),
        }
    }

    fn take_endpoint(&self) -> Option<waddle_xmpp::registry::ConnectionEntry> {
        match self.endpoint.lock() {
            Ok(mut staged) => staged.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }

    fn register(
        &self,
        jid: &FullJid,
        binding: &waddle_xmpp::ownership::BindingRef,
        resumed: bool,
    ) -> BindingPublication<RegistryPublication, RegistryRefusal> {
        let Some(entry) = self.take_endpoint() else {
            return BindingPublication::Refused(RegistryRefusal::NoEndpoint);
        };
        if entry.binding_ref() != Some(binding) {
            return BindingPublication::Refused(RegistryRefusal::EndpointBindingMismatch);
        }
        match self
            .state
            .deps
            .protocol
            .connection_registry
            .register_socket_entry(jid.clone(), entry)
        {
            Some(owner) => BindingPublication::Published(RegistryPublication { owner, resumed }),
            None => BindingPublication::Refused(RegistryRefusal::NotRegistered),
        }
    }
}

/// The publisher every binding of this server goes through. Substituting the
/// account coordinator means returning its publisher here.
pub(super) fn binding_publisher(state: &WebSocketState) -> RegistryBindingPublisher<'_> {
    RegistryBindingPublisher::new(state)
}

impl BindingPublisher for RegistryBindingPublisher<'_> {
    type Published = RegistryPublication;
    type Resume = ();
    type Receipt = ();
    type Refusal = RegistryRefusal;
    type Withdrawn = waddle_xmpp::registry::ConnectionEntry;
    type RemoteRetirement = RemoteBindingRetirement;
    type Error = std::convert::Infallible;

    async fn publish(
        &self,
        jid: &FullJid,
        binding: &waddle_xmpp::ownership::BindingRef,
        _lifetime: waddle_xmpp::ownership::joint_identity::ResourceLifetime,
    ) -> Result<BindingPublication<Self::Published, Self::Refusal>, Self::Error> {
        Ok(self.register(jid, binding, false))
    }

    async fn publish_resumed(
        &self,
        jid: &FullJid,
        binding: &waddle_xmpp::ownership::BindingRef,
        (): Self::Resume,
    ) -> Result<BindingPublication<Self::Published, Self::Refusal>, Self::Error> {
        Ok(self.register(jid, binding, true))
    }

    /// ADR-0017 Phase 1 completion: registration into the actor tree is
    /// authoritative and fail-closed. The mirror shares the same `Arc`-backed
    /// `ConnectionEntry` that was published (obtained owner-gated, carrying
    /// the presence and stream id set after publication), so the actor sees
    /// live presence and carbons updates without per-site mirroring.
    ///
    /// `NotCurrent` means the slot was already superseded by a racing
    /// reconnect on the same resource: the replacement owns the slot and
    /// performs its own authoritative register, so there is nothing for this
    /// binding to mirror.
    async fn confirm(
        &self,
        jid: &FullJid,
        published: &Self::Published,
    ) -> Result<BindingConfirmation<Self::Receipt, Self::Refusal>, Self::Error> {
        let state = self.state;
        let owner = published.owner;
        let Some(entry) = state
            .deps
            .protocol
            .connection_registry
            .entry_if_owner(jid, &owner)
        else {
            return Ok(BindingConfirmation::NotCurrent);
        };
        let mirror_outcome = if published.resumed {
            retry_resumed_registration_busy(|| {
                crate::server::dual_registration::mirror_register_outcome(
                    &state.deps.protocol.user_registry,
                    jid.clone(),
                    entry.clone(),
                )
            })
            .await
        } else {
            crate::server::dual_registration::mirror_register_outcome(
                &state.deps.protocol.user_registry,
                jid.clone(),
                entry.clone(),
            )
            .await
        };
        let outcome = match mirror_outcome {
            MirrorRegisterOutcome::ForeignOwner => {
                #[cfg(test)]
                if let Ok((entered, release)) = FOREIGN_OWNER_GATE.try_with(Clone::clone) {
                    entered.notify_one();
                    release.notified().await;
                }
                register_remote_clustered_resource(state, jid, entry, owner).await
            }
            outcome => outcome,
        };
        Ok(match outcome {
            MirrorRegisterOutcome::Registered => BindingConfirmation::Confirmed(()),
            refused => BindingConfirmation::Refused(RegistryRefusal::Mirror(refused)),
        })
    }

    /// Removing the entry also clears the presence state published with the
    /// binding.
    fn withdraw(&self, jid: &FullJid, published: &Self::Published) -> Option<Self::Withdrawn> {
        self.state
            .deps
            .protocol
            .connection_registry
            .unregister_if_owner(jid, &published.owner)
    }

    /// Unregister the binding from the account's actor tree, owner-gated so
    /// a racing replacement is left untouched. kameo's reply timeout does
    /// not cancel an already-enqueued handler, so a register that timed out
    /// may still land in the actor tree after its bind failed; the
    /// `UserRegistryActor` mailbox is FIFO, so this unregister is ordered
    /// after that register and prunes the phantom if it ran late. The
    /// registry has no use for the teardown evidence.
    async fn retire(
        &self,
        jid: &FullJid,
        published: &Self::Published,
        _evidence: RetireEvidence,
    ) -> Result<BindingRetirement<Self::Refusal>, Self::Error> {
        use crate::server::dual_registration::MirrorUnregisterOutcome;
        Ok(
            match crate::server::dual_registration::mirror_unregister(
                &self.state.deps.protocol.user_registry,
                jid,
                Some(published.owner),
            )
            .await
            {
                MirrorUnregisterOutcome::Completed => BindingRetirement::Retired,
                refused => BindingRetirement::Refused(RegistryRefusal::MirrorUnregister(refused)),
            },
        )
    }

    async fn retire_remote(
        &self,
        jid: &FullJid,
        published: &Self::Published,
        _evidence: RetireEvidence,
    ) -> Result<Self::RemoteRetirement, Self::Error> {
        Ok(unregister_remote_clustered_resource_if_owner(self.state, jid, &published.owner).await)
    }
}

/// What a binding's teardown established when the connection has no
/// account-level evidence to give: the work it accepted is owned by the
/// stream-management and offline recovery paths, and nothing is known to be
/// outstanding for directed presence or one-time presentations.
pub(super) fn recovery_owned_retirement() -> RetireEvidence {
    use waddle_xmpp::ownership::account_resource_transition::{
        AccountResourceOperationId, BindingRetirementDisposition,
        DirectedPresenceTeardownDisposition, PresentationFlushDisposition,
    };
    RetireEvidence {
        begin: AccountResourceOperationId::fresh(),
        complete: AccountResourceOperationId::fresh(),
        custody_disposition: BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
        directed_presence:
            DirectedPresenceTeardownDisposition::RecipientsRetractedAndUnavailableRecoveryOwned,
        presentation_flush: PresentationFlushDisposition::RecoveryOwner,
    }
}

/// Withdraw and retire a binding whose publication will not be acknowledged.
async fn rollback_registered_connection(
    state: &WebSocketState,
    jid: &FullJid,
    published: &RegistryPublication,
    conn: &mut WsConnState,
) {
    conn.retire_binding_ref();
    let publisher = binding_publisher(state);
    let _ = publisher.withdraw(jid, published);
    report_retirement(
        jid,
        publisher
            .retire(jid, published, recovery_owned_retirement())
            .await,
    );
    let _ = publisher
        .retire_remote(jid, published, recovery_owned_retirement())
        .await;
    conn.registry_owner = None;
}

/// Log a retirement the account's owner did not complete. The binding is
/// already out of the routing index; what remains is the owner's record.
pub(super) fn report_retirement<R: std::fmt::Debug, E: std::fmt::Debug>(
    jid: &FullJid,
    outcome: Result<BindingRetirement<R>, E>,
) {
    match outcome {
        Ok(BindingRetirement::Retired | BindingRetirement::NotCurrent { .. }) => {}
        Ok(BindingRetirement::Refused(refusal)) => {
            tracing::warn!(%jid, ?refusal, "the account's owner refused a binding retirement");
        }
        Err(error) => {
            tracing::warn!(%jid, ?error, "a binding retirement has an unknown outcome");
        }
    }
}

#[cfg(feature = "clustering")]
async fn unregister_remote_clustered_resource_if_owner(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
) -> RemoteBindingRetirement {
    use crate::clustering::route_bridge::RemoteResourceUnregisterOutcome;
    RemoteBindingRetirement::Relay(
        match state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
        {
            Some(bridge) => {
                bridge
                    .unregister_remote_user_resource_if_owner(jid, owner)
                    .await
            }
            None => RemoteResourceUnregisterOutcome::NotRegistered,
        },
    )
}

#[cfg(not(feature = "clustering"))]
async fn unregister_remote_clustered_resource_if_owner(
    _state: &WebSocketState,
    _jid: &FullJid,
    _owner: &waddle_xmpp::registry::LocalConnectionBindingId,
) -> RemoteBindingRetirement {
    RemoteBindingRetirement::NoRemoteOwner
}

/// Single choke point for a failed post-auth session initialization (#1454).
///
/// Every `SessionInitializationFailed` return routes through here so the
/// client-visible stream `<internal-server-error/>` is never server-silent
/// again: one `error!` with the bare JID, the XEP-0198 stream id when one
/// exists (fresh binds enable SM only after bind, so it is empty there),
/// the typed reason, and the failure detail; the
/// `waddle.session.init.failed` counter keyed by that reason so the rate is
/// alertable; and a dedicated error-status span. The span is created here —
/// not recorded on `Span::current()` — because registration runs after
/// `handle_xmpp_frame` returns, outside `xmpp.stanza.dispatch`, where a
/// current-span mark would be a silent no-op; a short point-in-time root
/// span is exactly the #1428 "internal failures export as error spans"
/// shape. The bare JID lives on the log and span only — never as a metric
/// attribute.
pub(super) fn record_session_init_failure(
    reason: waddle_xmpp::telemetry::attributes::SessionInitFailureReason,
    jid: &FullJid,
    stream_id: Option<&str>,
    detail: Option<String>,
) {
    use waddle_xmpp::telemetry::attributes::MetricAttribute;
    // `error_span!` (not `info_span!`): the registry-level `EnvFilter` gates
    // span export too, so an operator running `RUST_LOG=warn`/`error` must
    // not silently lose the failure span while keeping the log line.
    let span = tracing::error_span!(
        "xmpp.session.init",
        user = %jid.to_bare(),
        reason = reason.value(),
    );
    let _enter = span.enter();
    // Inside the span scope so the OTLP log record carries this span's
    // trace/span id — the alertable line pivots straight to the Tempo
    // error span.
    error!(
        user = %jid.to_bare(),
        resource = %jid.resource(),
        // Logged as an Option so a fresh bind (no SM yet → None) stays
        // distinguishable from a genuinely empty id.
        stream_id = ?stream_id,
        reason = reason.value(),
        detail = detail.as_deref().unwrap_or(""),
        "session initialization failed; sending stream internal-server-error and closing"
    );
    waddle_xmpp::counter_add!(
        "waddle.session.init.failed",
        "1",
        "Post-auth session initialization failures by reason; each one sent a \
         client a stream-level internal-server-error.",
        1,
        reason,
    );
    crate::telemetry::mark_span_error("session initialization failed");
}

/// Publish the SM stream id onto the freshly-registered entry so the
/// offline-flush path keys claims by the XEP-0198 session id, not the
/// resource JID. For a fresh bind without SM enabled, sm_state.stream_id is
/// None and the flush path falls back to delete-on-push for non-SM sessions.
///
/// This — and the presence publication below — run BEFORE the authoritative
/// mirror `ask`, not after (concurrency review, Slice 0): the mirror is a
/// blocking `ask` bounded at 2s, and leaving it between the DashMap
/// `register` and these mutations would leave the just-bound resource
/// registered-but-unavailable (and stream-id-less) on the authoritative
/// routing map for the whole ask window. On an SM resume that window would
/// hide a live resource from RFC 6121 §8.5.2.1.1 bare-JID selection. Setting
/// presence/stream-id first closes it; the actor shares this same
/// `Arc`-backed entry, so it still observes these atomics.
pub(super) fn publish_stream_id_and_presence(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &waddle_xmpp::registry::LocalConnectionBindingId,
    conn: &WsConnState,
) {
    // Owner-gated: a racing same-JID replacement can take the slot between
    // `register_with_stream_state` and this publication; a stale-owner write
    // would stamp OUR stream id and restored presence onto the replacement's
    // entry / the JID-keyed presence map. Must go through the registry's
    // owner-gated setter (not the raw entry method) so the
    // `sm_stream_owners` index is maintained: SM claim demotion resolves the
    // attached connection through that index, and a resumed stream missing
    // from it would be demoted without its termination signal.
    if state
        .deps
        .protocol
        .connection_registry
        .set_sm_stream_id_if_owner(
            jid,
            owner,
            conn.sm_state
                .stream_id
                .clone()
                .map(waddle_xmpp::pending_delivery::SmSessionId::new),
        )
    {
        // Resumable-only for the same reason as the enable site: only a
        // resumable stream carries a claim fence.
        if conn.sm_state.is_resumable() {
            super::stream_management::terminate_if_demoted_during_publication(
                state,
                jid,
                owner,
                conn.sm_state
                    .stream_id
                    .clone()
                    .map(waddle_xmpp::pending_delivery::SmSessionId::new)
                    .as_ref(),
            );
        }
    }

    // Each write re-verifies ownership INSIDE the registry call: a separate
    // `entry_if_owner(...).is_some()` check ahead of ungated writes would let
    // a same-JID replacement that registers between check and write inherit
    // OUR availability/presence. The owner-gated variants hold the entry
    // guard across check and write, so a refused write means the replacement
    // owns the slot and its own initial presence supersedes ours.
    if conn.presence_available {
        let registry = &state.deps.protocol.connection_registry;
        registry.update_presence_if_owner(jid, owner, true, conn.presence_priority);
        registry.update_presence_state_if_owner(
            jid,
            owner,
            conn.presence_show
                .as_ref()
                .map(sm_show_name)
                .map(str::to_string),
            conn.presence_status.clone(),
            conn.presence_priority,
            // XEP-0198 resume restores the full last presence,
            // extension payloads included (XEP-0115 caps, XEP-0319
            // idle) — RFC 6121 §4.3.2 requires probe responses to
            // reproduce the complete stanza, and the client sends no
            // new presence after <resumed/> (#1103 follow-up).
            conn.presence_payloads.clone(),
        );
    }
}

#[cfg(test)]
pub(super) async fn register_bound_connection_after_frame<E>(
    state: &WebSocketState,
    domain: &str,
    conn: &mut WsConnState,
    pending_tx: &mut Option<E>,
) -> RegistrationAfterFrame
where
    E: Into<SocketOutboundEndpoint>,
{
    let lifecycle = crate::clustering::NodeLifecycle::new();
    let permit = lifecycle.admit().expect("fresh serving lifecycle");
    let shutdown = tokio_util::sync::CancellationToken::new();
    register_bound_connection_after_frame_with_admission(
        state, domain, conn, pending_tx, &permit, &shutdown, None,
    )
    .await
}

pub(super) async fn register_bound_connection_after_frame_with_admission<E>(
    state: &WebSocketState,
    domain: &str,
    conn: &mut WsConnState,
    pending_tx: &mut Option<E>,
    permit: &NodeAdmissionPermit,
    shutdown: &tokio_util::sync::CancellationToken,
    outbound_receiver: Option<&SocketOutboundReceiver>,
) -> RegistrationAfterFrame
where
    E: Into<SocketOutboundEndpoint>,
{
    // Take the provisional resume identity before any early return or await.
    // It is reinstalled only if this registration reaches local publication.
    let pending_resume_binding = if conn.pending_resume_stream_id.is_some() {
        conn.binding_ref.take()
    } else {
        None
    };
    if !registration_authoritative(permit, shutdown) {
        return RegistrationAfterFrame::AuthorityRevoked;
    }
    let Some(jid) = conn.phase.bound_jid().cloned() else {
        return RegistrationAfterFrame::Unchanged;
    };
    if conn.resume_endpoint_handoff_blocked {
        error!(
            jid = %jid,
            "same-socket bind refused because failed-resume endpoint custody is unresolved"
        );
        // Match the pre-existing no-endpoint path and leave the connection
        // loop's receiver attached to the old queue. Its nonempty disposition
        // remains unresolved; converting this refusal into a session-init
        // failure would close the socket and drop that receiver immediately.
        return RegistrationAfterFrame::Unchanged;
    }
    if pending_tx.is_some() && conn.recovered_pending_endpoint.is_some() {
        error!(
            jid = %jid,
            "same-socket bind has two outbound endpoint custody sources"
        );
        return RegistrationAfterFrame::SessionInitializationFailed;
    }
    if pending_tx.is_none() && conn.recovered_pending_endpoint.is_none() {
        return RegistrationAfterFrame::Unchanged;
    }

    // Mirror the bind transition into the per-connection state machine (#229
    // PR11). The SM stays `None` until here so unauthenticated traffic can't
    // reach `on_peer_stanza`. We detect SM-resume vs fresh bind from whether
    // `pending_resume_stream_id` is waiting to be consumed below.
    let resumed = conn.pending_resume_stream_id.is_some();
    let binding_ref = if resumed {
        match pending_resume_binding {
            Some(binding) if binding.connection_ref() == &conn.connection_ref => binding,
            _ => {
                error!("resume binding identity is missing or belongs to another socket");
                return RegistrationAfterFrame::SessionInitializationFailed;
            }
        }
    } else {
        conn.retire_binding_ref();
        match conn.bindings.prepare_next() {
            Ok(prepared) => prepared.attempt.binding_ref,
            Err(refusal) => {
                error!(%refusal, "bind could not mint a binding identity");
                return RegistrationAfterFrame::SessionInitializationFailed;
            }
        }
    };

    let blocklist = if resumed {
        // XEP-0198 resume: the previous session was detached/dropped, but we
        // deliberately do NOT re-read from DB. Re-reading would let blocklist
        // mutations from other resources during the detach window leak into
        // the resumed stream, contradicting the snapshot semantic. The resumed
        // session starts with an empty snapshot; subsequent XEP-0191 IQ-set
        // traffic on the resumed stream re-populates it via the SM's internal
        // blocklist mutators.
        Blocklist::empty()
    } else {
        match load_blocklist_for_bind(&state.deps.app_state.db_pool, &jid).await {
            Ok(blocklist) => blocklist,
            Err(error) => {
                if !registration_authoritative(permit, shutdown) {
                    return RegistrationAfterFrame::AuthorityRevoked;
                }
                // Fail the bind rather than run the session with an empty
                // blocklist (a session-long XEP-0191 fail-open). The choke
                // point below is the single log line for this failure —
                // #1175 log hygiene: no second free-standing error!.
                record_session_init_failure(
                    waddle_xmpp::telemetry::attributes::SessionInitFailureReason::BlocklistLoad,
                    &jid,
                    conn.sm_state.stream_id.as_deref(),
                    Some(format!("XEP-0191 blocklist load failed: {error}")),
                );
                return RegistrationAfterFrame::SessionInitializationFailed;
            }
        }
    };

    if !registration_authoritative(permit, shutdown) {
        return RegistrationAfterFrame::AuthorityRevoked;
    }

    // The collector must outlive the bind and publication guards, including
    // when this future is cancelled during failed-resume cleanup.
    let mut failed_resume_wakeups = DeferredPendingFlushWakeups::default();
    let prepared = tokio::select! {
        _ = shutdown.cancelled() => return RegistrationAfterFrame::AuthorityRevoked,
        prepared = tokio::time::timeout(
            BIND_RETIREMENT_TIMEOUT,
            prepare_socket_bind(state, &jid, conn.occupancy_session, resumed),
        ) => prepared,
    };
    let mut bind_guard = match prepared {
        Ok(Ok(guard)) => guard,
        Ok(Err(BindPreparationError::ResumeAuthorityRevoked)) => {
            return RegistrationAfterFrame::Registered(
                reject_unregistered_resume(state, conn, &jid).await,
            );
        }
        failure => {
            let detail = match failure {
                Ok(Err(error)) => error.to_string(),
                Err(error) => format!("occupancy retirement timed out: {error}"),
                Ok(Ok(_)) => unreachable!("successful bind preparation handled above"),
            };
            record_session_init_failure(
                waddle_xmpp::telemetry::attributes::SessionInitFailureReason::AuthoritativeRegistration,
                &jid,
                conn.sm_state.stream_id.as_deref(),
                Some(detail),
            );
            return RegistrationAfterFrame::SessionInitializationFailed;
        }
    };
    if !registration_authoritative(permit, shutdown) {
        return RegistrationAfterFrame::AuthorityRevoked;
    }

    #[cfg(test)]
    if let Ok((entered, release)) = PREPARED_BIND_GATE.try_with(Clone::clone) {
        entered.notify_one();
        release.notified().await;
    }

    let publication = tokio::select! {
        _ = shutdown.cancelled() => return RegistrationAfterFrame::AuthorityRevoked,
        result = tokio::time::timeout(
            BIND_RETIREMENT_TIMEOUT,
            crate::occupancy_authority::acquire_current(
                state.deps.app_state.db_pool.global(), &jid, conn.occupancy_session,
            ),
        ) => result,
    };
    if resumed && matches!(publication, Ok(Ok(None))) {
        return RegistrationAfterFrame::Registered(
            reject_unregistered_resume(state, conn, &jid).await,
        );
    }
    let publication_guard = match publication {
        Ok(Ok(Some(guard))) => guard,
        failure => {
            let detail = match failure {
                Ok(Ok(None)) => {
                    "occupancy authority changed before registration publication".to_owned()
                }
                Ok(Err(error)) => format!(
                    "occupancy authority storage failed before registration publication: {error}"
                ),
                Err(error) => format!(
                    "occupancy authority check timed out before registration publication: {error}"
                ),
                Ok(Ok(Some(_))) => unreachable!("current publication guard handled above"),
            };
            record_session_init_failure(
                waddle_xmpp::telemetry::attributes::SessionInitFailureReason::AuthoritativeRegistration,
                &jid,
                conn.sm_state.stream_id.as_deref(),
                Some(detail),
            );
            return RegistrationAfterFrame::SessionInitializationFailed;
        }
    };

    conn.ensure_state_machine(
        domain,
        &state.deps.protocol.dispatcher,
        jid.clone(),
        resumed,
        blocklist,
    );

    let tx: SocketOutboundEndpoint = if let Some(tx) = pending_tx.take() {
        tx.into()
    } else if let Some(endpoint) = conn.recovered_pending_endpoint.take() {
        endpoint
    } else {
        return RegistrationAfterFrame::Unchanged;
    };

    // Only the attempt this connection last prepared may be published. A
    // resume carries the identity it minted when the attempt began.
    if !matches!(
        conn.bindings.current(),
        Some((current, BindingStage::Prepared)) if current == &binding_ref
    ) {
        error!(jid = %jid, "binding identity is not this connection's prepared attempt");
        return RegistrationAfterFrame::SessionInitializationFailed;
    }
    let published_binding = binding_ref.clone();
    conn.binding_ref = Some(binding_ref.clone());
    let mut entry = match &conn.local_io_relinquishment {
        Some(witness) => waddle_xmpp::registry::ConnectionEntry::new_for_binding_with_local_io(
            tx,
            binding_ref,
            witness.clone(),
        ),
        None => waddle_xmpp::registry::ConnectionEntry::new_for_binding(tx, binding_ref),
    };
    entry.write_interruption = conn.write_interruption.clone();
    if !entry.wire_resume_control(jid.clone(), conn.live_resume_control.clone()) {
        conn.retire_binding_ref();
        return RegistrationAfterFrame::SessionInitializationFailed;
    }
    entry
        .carbons_enabled
        .store(conn.carbons_enabled, std::sync::atomic::Ordering::Relaxed);
    entry
        .roster_interested
        .store(conn.roster_interested, std::sync::atomic::Ordering::Relaxed);
    entry.blocklist_interested.store(
        conn.blocklist_interested,
        std::sync::atomic::Ordering::Relaxed,
    );
    let publisher = RegistryBindingPublisher::with_endpoint(state, entry);
    let publication = if resumed {
        publisher
            .publish_resumed(&jid, &published_binding, ())
            .await
    } else {
        publisher
            .publish(
                &jid,
                &published_binding,
                waddle_xmpp::ownership::joint_identity::ResourceLifetime {
                    logical: waddle_xmpp::ownership::endpoint_transition::LogicalResourceGeneration::fresh(),
                    occupancy: conn.occupancy_session,
                },
            )
            .await
    };
    let published = match publication {
        Ok(BindingPublication::Published(published)) => published,
        Ok(BindingPublication::ReplacementPending { .. }) => {
            error!(jid = %jid, "the connection registry reported a pending replacement");
            conn.retire_binding_ref();
            return RegistrationAfterFrame::SessionInitializationFailed;
        }
        Ok(BindingPublication::Refused(refusal)) => {
            tracing::debug!(jid = %jid, %refusal, "binding was not published");
            conn.retire_binding_ref();
            return RegistrationAfterFrame::SessionInitializationFailed;
        }
        Err(never) => match never {},
    };
    let owner = published.owner;
    let _ = conn.bindings.install_binding(&published_binding);
    conn.registry_owner = Some(owner);
    if let Some(entry) = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(&jid, &owner)
    {
        conn.socket_lifecycle = Some(bind_guard.publish(entry, conn.occupancy_session));
    }
    // Publish the occupancy generation (#1703) owner-gated, like the SM
    // stream id below.
    let _ = state
        .deps
        .protocol
        .connection_registry
        .set_occupancy_session_if_owner(&jid, &owner, conn.occupancy_session);

    // Publish the SM stream id + restored presence onto the
    // freshly-registered entry, owner-gated against a racing same-JID
    // replacement (#1139 resume-claim clobber fix — see
    // `publish_stream_id_and_presence`'s doc comment for the full
    // "run before the authoritative mirror ask" rationale).
    publish_stream_id_and_presence(state, &jid, &owner, conn);

    // Only synchronous routing publication needs the SQL authority fence.
    // Keep the per-JID bind gate while mirrors drain, but return the main-pool
    // connection before actor, relay, or rollback awaits. A remote successor
    // is detected again after SM finalization below.
    drop(publication_guard);

    #[cfg(test)]
    if let Ok((entered, release)) = LOCAL_PUBLICATION_GATE.try_with(Clone::clone) {
        entered.notify_one();
        release.notified().await;
    }

    if !registration_authoritative(permit, shutdown) {
        rollback_registered_connection(state, &jid, &published, conn).await;
        return RegistrationAfterFrame::AuthorityRevoked;
    }

    if resumed && conn.pending_subscribes_flushed {
        // XEP-0198 §5: a resumed stream is the SAME session. The
        // detached session recorded that its once-per-session
        // pending-subscribe delivery already fired (RFC 6121 §3.1.3),
        // so consume the fresh entry's claim — a presence flip after
        // resume must not re-prompt (#1104 follow-up). Gated on the
        // carried flag, NOT on current presence: a session that went
        // available (claim consumed) then unavailable before detaching
        // must not re-arm the claim on resume. Subscribes that arrived
        // during the detached window were fanned out to the detached
        // session and reach the client via SM replay instead.
        // Owner-gated: a racing same-JID replacement must not have ITS
        // once-per-session claim consumed by this resume's pre-claim.
        if let Some(entry) = state
            .deps
            .protocol
            .connection_registry
            .entry_if_owner(&jid, &owner)
        {
            let _ = entry.claim_pending_subscribes_flush();
        }
    }

    // If the account's owner cannot confirm the resource, the publication is
    // rolled back (owner-gated, so a racing replacement is left intact) and
    // the bind fails: a lagging register is a *silent false-negative* — a
    // bare-JID selection that misses this live resource looks like a complete
    // set and can never fall back — so the two views must never disagree in
    // the miss-a-resource direction. The client simply reconnects. See
    // docs/adrs/0017-phase1-completion-authoritative-registration.md.
    let confirmation = match binding_publisher(state).confirm(&jid, &published).await {
        Ok(confirmation) => confirmation,
        Err(never) => match never {},
    };
    if !matches!(confirmation, BindingConfirmation::NotCurrent) {
        if !registration_authoritative(permit, shutdown) {
            rollback_registered_connection(state, &jid, &published, conn).await;
            return RegistrationAfterFrame::AuthorityRevoked;
        }
        match confirmation {
            BindingConfirmation::Confirmed(()) | BindingConfirmation::NotCurrent => {
                bind_guard.complete_actor_retirements_after_registration();
            }
            BindingConfirmation::Refused(refusal) => {
                conn.retire_binding_ref();
                let publisher = binding_publisher(state);
                let _ = publisher.withdraw(&jid, &published);
                report_retirement(
                    &jid,
                    publisher
                        .retire(&jid, &published, recovery_owned_retirement())
                        .await,
                );
                conn.registry_owner = None;
                // The rollback ran and the client gets a stream error; this
                // record is the server-side trace of it (#1454).
                record_session_init_failure(
                    waddle_xmpp::telemetry::attributes::SessionInitFailureReason::AuthoritativeRegistration,
                    &jid,
                    conn.sm_state.stream_id.as_deref(),
                    None,
                );
                return if matches!(
                    refusal,
                    RegistryRefusal::Mirror(
                        MirrorRegisterOutcome::Busy | MirrorRegisterOutcome::ForeignOwner
                    )
                ) {
                    RegistrationAfterFrame::RegistrationTemporarilyUnavailable
                } else {
                    RegistrationAfterFrame::SessionInitializationFailed
                };
            }
        }
    }

    #[cfg(feature = "test-support")]
    super::characterization_seams::pause_published_resume_finalization(state, conn, &jid, &owner)
        .await;
    let sm_report = finalize_sm_after_registry_registration(
        state,
        conn,
        &jid,
        &owner,
        outbound_receiver,
        &mut failed_resume_wakeups,
    )
    .await;
    #[cfg(test)]
    if let Some((reached, release)) = conn.post_sm_finalization_test_hook.take() {
        reached.notify_one();
        release.notified().await;
    }
    let generation_current = conn.phase.bound_jid().is_none()
        || crate::occupancy_authority::is_current(
            state.deps.app_state.db_pool.global(),
            &jid,
            conn.occupancy_session,
        )
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%jid, %error,
                "occupancy authority lookup failed after stream finalization");
            false
        });
    if !registration_authoritative(permit, shutdown) || !generation_current {
        // `complete_pending_resume_claim` persist-deletes the detached
        // snapshot after restoring it into `conn.sm_state`. Clearing the
        // registry owner here would make outer shutdown cleanup skip detach
        // and lose that recovered queue. Preserve the owner and let the
        // normal cleanup path re-store the live session before unregister.
        // The finalized resume result is deliberately NOT recorded on this
        // path: the client never receives the finalized response.
        return RegistrationAfterFrame::AuthorityRevokedAfterSmFinalization;
    }
    // Not recorded yet: the writer's own authority preflight can still drop
    // the response batch. The main loop records this only after the batch
    // carrying the terminal frame is actually written.
    conn.pending_finalized_resume_outcome = sm_report.resume_outcome;
    info!(
        jid = %jid,
        resumed = conn.phase.is_resumed(),
        carbons_enabled = conn.carbons_enabled,
        "WebSocket connection registered"
    );

    RegistrationAfterFrame::Registered(sm_report.finalization)
}

#[cfg(feature = "clustering")]
async fn register_remote_clustered_resource(
    state: &WebSocketState,
    jid: &FullJid,
    entry: waddle_xmpp::registry::ConnectionEntry,
    owner: waddle_xmpp::registry::LocalConnectionBindingId,
) -> MirrorRegisterOutcome {
    use crate::server::dual_registration::MirrorRegisterOutcome;
    let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    else {
        return MirrorRegisterOutcome::Failed;
    };

    // A foreign claim can expire between the initial mirror and the relay
    // lookup, or while the failed relay exchange is in flight. NotRemote
    // returns us to the local registry, whose OwnerStale CAS remains the sole
    // takeover authority. Bound owner-movement races rather than spinning
    // across an unstable claim or falling through to an unregistered socket.
    for attempt in 0..RESUME_REGISTRATION_BUSY_ATTEMPTS {
        match bridge
            .try_register_remote_user_resource(jid, entry.clone(), owner)
            .await
        {
            RemoteResourceRegisterOutcome::Registered => {
                return MirrorRegisterOutcome::Registered;
            }
            RemoteResourceRegisterOutcome::Retryable => {
                return MirrorRegisterOutcome::Busy;
            }
            RemoteResourceRegisterOutcome::Failed => {
                return MirrorRegisterOutcome::Failed;
            }
            RemoteResourceRegisterOutcome::NotRemote => {
                let outcome = retry_resumed_registration_busy(|| {
                    crate::server::dual_registration::mirror_register_outcome(
                        &state.deps.protocol.user_registry,
                        jid.clone(),
                        entry.clone(),
                    )
                })
                .await;
                if outcome != MirrorRegisterOutcome::ForeignOwner {
                    return outcome;
                }
                if attempt + 1 < RESUME_REGISTRATION_BUSY_ATTEMPTS {
                    tokio::time::sleep(resume_registration_busy_backoff(attempt)).await;
                }
            }
        }
    }
    MirrorRegisterOutcome::Busy
}

#[cfg(not(feature = "clustering"))]
async fn register_remote_clustered_resource(
    _state: &WebSocketState,
    _jid: &FullJid,
    _entry: waddle_xmpp::registry::ConnectionEntry,
    _owner: waddle_xmpp::registry::LocalConnectionBindingId,
) -> MirrorRegisterOutcome {
    MirrorRegisterOutcome::Failed
}
