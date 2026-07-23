//! Instrumented, bounded handle to the [`RoomRegistryActor`].
//!
//! The #757 production incident was a wedged `RoomRegistryActor`: callers that
//! `.ask()`ed it blocked forever, freezing the per-connection frame loop with no
//! actionable signal. This handle is fix-3 of that incident series. It wraps a
//! plain [`ActorRef<RoomRegistryActor>`] and makes every request:
//!
//! - **bounded in time** — a [`ROOM_REGISTRY_REPLY_TIMEOUT`] reply timeout turns
//!   an indefinite hang into a typed [`RoomRegistryError::Timeout`] in seconds;
//! - **observable** — per-request latency is recorded
//!   ([`metrics::record_actor_mailbox_latency`]) and a `warn!` fires when a
//!   request exceeds [`ROOM_REGISTRY_SLOW_ASK_WARN`];
//! - **typed on failure** — mailbox/transport failures map to typed
//!   [`RoomRegistryError`] variants rather than stringly-typed diagnostics.
//!
//! The actor is spawned with an **explicit named bounded mailbox**
//! ([`ROOM_REGISTRY_MAILBOX_CAPACITY`]); a saturated mailbox produces a typed
//! error instead of unbounded growth, and the remaining capacity feeds the
//! periodic mailbox-depth gauge.
//!
//! This handle is the *fast, specific* fail-path for the known actor wedge,
//! complementary to the coarse per-stanza frame backstop (#808): see
//! `docs/adr/008-stanza-handler-wedge-backstop.md`.

use std::sync::Arc;
use std::time::Duration;

use jid::BareJid;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use kameo::mailbox;
use tokio::time::Instant;
use tracing::warn;

use super::affiliation::DurableMembershipSource;
use super::durable::MucDurableStore;
use super::room_actor::{RoomActor, SealGuard};
use super::room_registry_actor::{
    AbandonRoomOwnershipForShutdown, CancelPendingReclaimedRoomReservation, CreateInstantRoom,
    CreateRoom, DemoteRoomIfOwner, DestroyRoom, DestroyRoomIfInactive,
    DestroyRoomIfInactiveOutcome, DestroyRoomOutcome, DestroyRoomReason,
    DrainRoomOwnershipForShutdown, GetOrCreateRoom, GetPendingReclaimedRoomBacklog,
    GetPendingRoomReleaseBacklog, GetRoom, IsCurrentIdentityPendingRoomReleaseOnly,
    IsCurrentRoomPendingRelease, IsMucJid, IsPendingRoomReleaseOnly, ListPendingReclaimedRooms,
    ListPendingRoomReleaseJids, ListRooms, ListRoomsOwnedBy, PendingReclaimedRoom,
    PendingReclaimedRoomBacklog, PendingRoomReleaseBacklog, ReapSealedRoom, ReclaimedRoomOutcome,
    ReconcileReclaimedRoom, RememberPendingReclaimedRoom, ReservePendingReclaimedRoom,
    RetryPendingRoomReleases, RoomAcquisition, RoomCount, RoomExists, RoomOwnershipDrainOutcome,
    RoomRegistryActor, RoomRegistryError, WireClusteringClaims,
};
use super::RoomConfig;
use crate::metrics;
use crate::ownership::{ClaimStore, NodeIdentity, SharedNodeIdentity};
use crate::xep::xep0421::OccupantIdSecret;

/// Explicit bounded mailbox capacity for the `RoomRegistryActor`.
///
/// kameo's implicit default is 64; we name it (and raise it slightly) so the
/// value is reviewable and the depth gauge has a stable denominator. 128 gives
/// burst headroom for reconnection storms while keeping a wedged actor's backlog
/// bounded and observable rather than growing without limit.
pub const ROOM_REGISTRY_MAILBOX_CAPACITY: usize = 128;

/// Hard fail-fast budget for a single registry request.
///
/// Chosen well above normal handler latency (sub-millisecond in practice) so it
/// never trips on healthy load, yet comfortably below the #808 per-stanza frame
/// backstop (15s) so a wedged registry surfaces as a typed error long before the
/// coarse frame timeout would fire.
pub const ROOM_REGISTRY_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Hard fail-fast budget for *enqueuing* a request into the bounded mailbox.
///
/// `reply_timeout` only bounds the wait for the actor's reply — not the wait for
/// free mailbox capacity. When a wedged actor's 128-slot mailbox saturates, a
/// caller would otherwise block indefinitely on `send` before its request is
/// even enqueued. Bounding the enqueue wait too means a saturated mailbox
/// surfaces as a typed [`RoomRegistryError`] instead of freezing the caller.
/// Same budget as the reply timeout; worst-case total stays below the #808 15s
/// frame backstop.
pub const ROOM_REGISTRY_MAILBOX_TIMEOUT: Duration = Duration::from_secs(5);

/// Latency above which a single registry request is logged at `warn!` — a
/// leading indicator of saturation before the hard [`ROOM_REGISTRY_REPLY_TIMEOUT`].
pub const ROOM_REGISTRY_SLOW_ASK_WARN: Duration = Duration::from_millis(500);

/// Metrics actor label for the registry (kept stable for dashboards/alerts).
const ACTOR_LABEL: &str = "room_registry";

/// Cheap-clone, instrumented handle to the MUC room registry actor.
#[derive(Clone)]
pub struct RoomRegistry {
    inner: ActorRef<RoomRegistryActor>,
    max_capacity: usize,
}

impl RoomRegistry {
    /// Spawn the registry actor behind an explicit bounded mailbox and return an
    /// instrumented handle to it.
    ///
    /// `membership_source` hydrates every freshly spawned `RoomActor`'s
    /// durable inbox recipient set from the deployment's durable
    /// membership store (#1135). Pass `None` only when no durable
    /// membership store exists (tests, tools); production deployments
    /// must wire one or offline members drop out of groupchat inbox
    /// fan-out after each room-actor respawn.
    pub fn spawn(
        muc_domain: String,
        occupant_id_secret: OccupantIdSecret,
        membership_source: Option<Arc<dyn DurableMembershipSource>>,
    ) -> Self {
        let mut actor = RoomRegistryActor::new(muc_domain, occupant_id_secret);
        if let Some(source) = membership_source {
            actor = actor.with_membership_source(source);
        }
        let inner = RoomRegistryActor::spawn_with_mailbox(
            actor,
            mailbox::bounded(ROOM_REGISTRY_MAILBOX_CAPACITY),
        );
        Self {
            inner,
            max_capacity: ROOM_REGISTRY_MAILBOX_CAPACITY,
        }
    }

    /// Wrap an existing actor ref (e.g. one already stored in shared state) so
    /// callers can use the instrumented typed methods without re-spawning.
    pub fn from_actor_ref(inner: ActorRef<RoomRegistryActor>, max_capacity: usize) -> Self {
        Self {
            inner,
            max_capacity,
        }
    }

    /// Wrap a shared `ActorRef` with the deployment's default mailbox capacity.
    ///
    /// Convenience for production call sites that hold the raw
    /// `ActorRef<RoomRegistryActor>` from shared state and want the instrumented
    /// typed methods (reply + mailbox timeout, typed errors, latency metrics) per
    /// request, without threading the capacity constant through every site. The
    /// capacity only feeds [`RoomRegistry::mailbox_depth`] (the gauge), so the
    /// spawn-time default is correct for the ask path.
    pub fn wrap(inner: ActorRef<RoomRegistryActor>) -> Self {
        Self::from_actor_ref(inner, ROOM_REGISTRY_MAILBOX_CAPACITY)
    }

    /// The underlying actor ref, for the few call sites that still need it
    /// directly (e.g. test fixtures).
    pub fn actor_ref(&self) -> &ActorRef<RoomRegistryActor> {
        &self.inner
    }

    /// Whether the registry actor is still running.
    pub fn is_alive(&self) -> bool {
        self.inner.is_alive()
    }

    /// The mailbox's configured capacity (denominator for the depth gauge).
    pub fn max_capacity(&self) -> i64 {
        self.max_capacity as i64
    }

    /// Current mailbox depth (queued messages), or `None` if the mailbox is
    /// unbounded (never the case for the spawned registry). Depth is
    /// `capacity - remaining`, where the bounded `MailboxSender` reports the
    /// remaining free slots.
    pub fn mailbox_depth(&self) -> Option<i64> {
        self.inner
            .mailbox_sender()
            .capacity()
            .map(|remaining| self.max_capacity().saturating_sub(remaining as i64))
    }

    /// Record latency / slow-warn on success and classify a request outcome into
    /// a typed [`RoomRegistryError`]. `elapsed` is measured by the caller around
    /// the `.ask(..).reply_timeout(..).await`.
    ///
    /// Generic over the message param `M` and handler error `E`: kameo's
    /// `ask().reply_timeout()` yields `SendError<Msg, RoomRegistryError>` for the
    /// `Result`-reply handlers and `SendError<Msg, Infallible>` for the plain-reply
    /// ones, so `E: Into<RoomRegistryError>` (with the `From<Infallible>` impl
    /// below) unifies both.
    fn classify<R, M, E>(
        operation: &'static str,
        elapsed: Duration,
        result: Result<R, SendError<M, E>>,
    ) -> Result<R, RoomRegistryError>
    where
        E: Into<RoomRegistryError>,
    {
        // A completed round-trip (the actor processed the request and replied,
        // with a value OR a typed handler error) is a representative latency
        // sample; record both so a slow handler that *errors* still appears on
        // P95/P99 dashboards. Only Timeout/transport-drop — where the actor
        // never replied — are excluded.
        let record_round_trip = |outcome: &str| {
            metrics::record_actor_mailbox_latency(
                ACTOR_LABEL,
                operation,
                "ask",
                elapsed.as_secs_f64() * 1000.0,
            );
            if elapsed >= ROOM_REGISTRY_SLOW_ASK_WARN {
                warn!(
                    operation,
                    outcome,
                    elapsed_ms = elapsed.as_millis() as u64,
                    "RoomRegistryActor request slow"
                );
            }
        };

        match result {
            Ok(reply) => {
                record_round_trip("ok");
                Ok(reply)
            }
            Err(SendError::HandlerError(error)) => {
                record_round_trip("handler_error");
                Err(error.into())
            }
            Err(SendError::Timeout(_)) => {
                metrics::record_actor_request_timeout(ACTOR_LABEL, operation, "ask");
                warn!(
                    operation,
                    timeout_ms = ROOM_REGISTRY_REPLY_TIMEOUT.as_millis() as u64,
                    "RoomRegistryActor request timed out"
                );
                Err(RoomRegistryError::Timeout)
            }
            Err(other) => {
                metrics::record_actor_request_dropped(
                    ACTOR_LABEL,
                    operation,
                    "ask",
                    send_error_reason(&other),
                );
                warn!(operation, "RoomRegistryActor request dropped");
                Err(RoomRegistryError::Unavailable)
            }
        }
    }
}

/// Plain-reply registry handlers (`bool`/`usize`/`()`) carry kameo's own
/// [`kameo::error::Infallible`] (an uninhabited enum, distinct from
/// `std::convert::Infallible`) as the handler-error type. This lets
/// [`RoomRegistry::classify`] accept both those and the `Result`-reply handlers
/// (`RoomRegistryError`) under one `E: Into<RoomRegistryError>` bound.
impl From<kameo::error::Infallible> for RoomRegistryError {
    fn from(never: kameo::error::Infallible) -> Self {
        match never {}
    }
}

/// Generates an instrumented async method per registry message: each issues the
/// `.ask(..)` under [`ROOM_REGISTRY_REPLY_TIMEOUT`], times it, and routes the
/// outcome through [`RoomRegistry::classify`]. Keeping this declarative avoids a
/// generic abstraction over kameo's message-specific request builders.
macro_rules! registry_method {
    (
        $(#[$meta:meta])*
        $name:ident ( $( $arg:ident : $arg_ty:ty ),* ) -> $reply:ty,
        $op:literal,
        $msg:expr
    ) => {
        $(#[$meta])*
        pub async fn $name(&self, $( $arg : $arg_ty ),* ) -> Result<$reply, RoomRegistryError> {
            let started = Instant::now();
            let result = self
                .inner
                .ask($msg)
                .mailbox_timeout(ROOM_REGISTRY_MAILBOX_TIMEOUT)
                .reply_timeout(ROOM_REGISTRY_REPLY_TIMEOUT)
                .await;
            Self::classify($op, started.elapsed(), result)
        }
    };
}

impl RoomRegistry {
    registry_method!(
        /// Look up a room actor by JID.
        get_room(room_jid: BareJid) -> Option<ActorRef<RoomActor>>,
        "get_room",
        GetRoom { room_jid }
    );

    registry_method!(
        reserve_pending_reclaimed_room(room_jid: BareJid) -> bool,
        "reserve_pending_reclaimed_room",
        ReservePendingReclaimedRoom { room_jid }
    );

    registry_method!(
        pending_reclaimed_room_backlog() -> PendingReclaimedRoomBacklog,
        "pending_reclaimed_room_backlog",
        GetPendingReclaimedRoomBacklog
    );

    registry_method!(
        drain_room_ownership_for_shutdown(
            pending_handoffs: Vec<PendingReclaimedRoom>
        ) -> RoomOwnershipDrainOutcome,
        "drain_room_ownership_for_shutdown",
        DrainRoomOwnershipForShutdown { pending_handoffs }
    );

    registry_method!(
        abandon_room_ownership_for_shutdown(
            pending_handoffs: Vec<PendingReclaimedRoom>
        ) -> RoomOwnershipDrainOutcome,
        "abandon_room_ownership_for_shutdown",
        AbandonRoomOwnershipForShutdown { pending_handoffs }
    );

    registry_method!(
        pending_room_release_backlog() -> PendingRoomReleaseBacklog,
        "pending_room_release_backlog",
        GetPendingRoomReleaseBacklog
    );

    registry_method!(
        retry_pending_room_releases(limit: usize) -> usize,
        "retry_pending_room_releases",
        RetryPendingRoomReleases { limit }
    );

    registry_method!(
        list_pending_room_release_jids() -> Vec<BareJid>,
        "list_pending_room_release_jids",
        ListPendingRoomReleaseJids
    );

    registry_method!(
        is_current_room_pending_release(room_jid: BareJid) -> bool,
        "is_current_room_pending_release",
        IsCurrentRoomPendingRelease { room_jid }
    );

    registry_method!(
        is_pending_room_release_only(room_jid: BareJid) -> bool,
        "is_pending_room_release_only",
        IsPendingRoomReleaseOnly { room_jid }
    );

    registry_method!(
        is_current_identity_pending_room_release_only(room_jid: BareJid) -> bool,
        "is_current_identity_pending_room_release_only",
        IsCurrentIdentityPendingRoomReleaseOnly { room_jid }
    );

    registry_method!(
        cancel_pending_reclaimed_room_reservation(room_jid: BareJid) -> (),
        "cancel_pending_reclaimed_room_reservation",
        CancelPendingReclaimedRoomReservation { room_jid }
    );

    registry_method!(
        /// Get an existing room or create one if absent. The reply's
        /// [`RoomAcquisition::creation`] bit is authoritative for the
        /// XEP-0045 §10.1.1 creator Owner grant (#1134).
        get_or_create_room(
            room_jid: BareJid,
            waddle_id: String,
            channel_id: String,
            config: RoomConfig
        ) -> RoomAcquisition,
        "get_or_create_room",
        GetOrCreateRoom { room_jid, waddle_id, channel_id, config }
    );

    registry_method!(
        /// Create a room, failing if one with the same JID already exists.
        create_room(
            room_jid: BareJid,
            waddle_id: String,
            channel_id: String,
            config: RoomConfig
        ) -> ActorRef<RoomActor>,
        "create_room",
        CreateRoom { room_jid, waddle_id, channel_id, config }
    );

    registry_method!(
        /// Create an instant room per XEP-0045. The reply's
        /// [`RoomAcquisition::creation`] bit is authoritative for the
        /// XEP-0045 §10.1.1 creator Owner grant (#1134).
        create_instant_room(room_jid: BareJid) -> RoomAcquisition,
        "create_instant_room",
        CreateInstantRoom { room_jid }
    );

    registry_method!(
        /// Destroy a room, returning the typed outcome. The handler
        /// removes the registry entry and wipes the room's clustering
        /// durable rows (config/subject/affiliations incl. bans) under
        /// one claim fence, restoring the entry and reporting
        /// [`DestroyRoomOutcome::DurableWipeFailed`] if the durable delete
        /// fails — the destroy is therefore all-or-nothing (#1261, #1276).
        destroy_room(room_jid: BareJid) -> DestroyRoomOutcome,
        "destroy_room",
        DestroyRoom {
            room_jid,
            reason: DestroyRoomReason::Destroy
        }
    );

    registry_method!(
        /// Destroy a room only if it is still inactive at the expected
        /// occupancy revision (#1108). Preserves a distinct ambiguity result
        /// when the inner room seal may have landed without a reply.
        destroy_room_if_inactive(
            room_jid: BareJid,
            expected_occupancy_revision: u64,
            guard: SealGuard
        ) -> DestroyRoomIfInactiveOutcome,
        "destroy_room_if_inactive",
        DestroyRoomIfInactive { room_jid, expected_occupancy_revision, guard }
    );

    registry_method!(
        /// Purge a sealed-but-registered room actor left behind by a
        /// timed-out guarded destroy (#1108 follow-up). Returns whether
        /// one was removed.
        reap_sealed_room(room_jid: BareJid) -> bool,
        "reap_sealed_room",
        ReapSealedRoom { room_jid }
    );

    registry_method!(
        /// Whether a room exists.
        room_exists(room_jid: BareJid) -> bool,
        "room_exists",
        RoomExists { room_jid }
    );

    registry_method!(
        /// Whether a bare JID belongs to this MUC service domain.
        is_muc_jid(jid: BareJid) -> bool,
        "is_muc_jid",
        IsMucJid { jid }
    );

    registry_method!(
        /// List all live room JIDs.
        list_rooms() -> Vec<BareJid>,
        "list_rooms",
        ListRooms
    );

    registry_method!(
        /// List rooms and pending release work tied to one exact owner.
        list_rooms_owned_by(owner: NodeIdentity) -> Vec<BareJid>,
        "list_rooms_owned_by",
        ListRoomsOwnedBy { owner }
    );

    registry_method!(
        /// Demote a live room only if its entry still belongs to `owner`.
        demote_room_if_owner(room_jid: BareJid, owner: NodeIdentity) -> bool,
        "demote_room_if_owner",
        DemoteRoomIfOwner { room_jid, owner }
    );

    registry_method!(
        /// Count active rooms.
        room_count() -> usize,
        "room_count",
        RoomCount
    );

    registry_method!(
        /// Serialize proactive orphan-claim adoption against demand-side
        /// room creation, hydrating durable rooms and releasing claims that
        /// have no restorable state.
        reconcile_reclaimed_room(
            room_jid: BareJid,
            claim_fence: super::RoomClaimFenceContext,
            previous_owner: crate::ownership::NodeIdentity
        ) -> ReclaimedRoomOutcome,
        "reconcile_reclaimed_room",
        ReconcileReclaimedRoom { room_jid, claim_fence, previous_owner }
    );

    registry_method!(
        /// Return a bounded page of reclaimed epochs awaiting actor
        /// installation or confirmed release. Each item is retried through
        /// `reconcile_reclaimed_room` as its own mailbox message.
        list_pending_reclaimed_rooms(limit: usize) -> Vec<PendingReclaimedRoom>,
        "list_pending_reclaimed_rooms",
        ListPendingReclaimedRooms { limit }
    );

    /// Enqueue a newly won epoch in the registry's idempotent retry set before
    /// beginning fallible adoption work. This intentionally waits only for
    /// mailbox acceptance, not a handler reply: an `ask` reply timeout is an
    /// uncertain commit and must never trigger concurrent exact release.
    pub async fn remember_pending_reclaimed_room(
        &self,
        room_jid: BareJid,
        claim_fence: super::RoomClaimFenceContext,
        previous_owner: crate::ownership::NodeIdentity,
    ) -> Result<(), RoomRegistryError> {
        self.inner
            .tell(RememberPendingReclaimedRoom {
                room_jid,
                claim_fence,
                previous_owner,
            })
            .mailbox_timeout(ROOM_REGISTRY_MAILBOX_TIMEOUT)
            .await
            .map_err(|error| {
                metrics::record_actor_request_dropped(
                    ACTOR_LABEL,
                    "remember_pending_reclaimed_room",
                    "tell",
                    send_error_reason(&error),
                );
                RoomRegistryError::Unavailable
            })
    }

    /// Wire the real clustering-backed claim store/identity/durable store
    /// into this already-spawned registry (ADR-0017 Phase 3 Slice 7). See
    /// [`WireClusteringClaims`]'s doc comment for the construction-order
    /// rationale. Fire-and-forget (`tell`, not `ask`): a failure here
    /// (mailbox saturated/actor stopped) is logged, not surfaced, since
    /// the caller (server startup) has no fallback action to take beyond
    /// what a `warn!` already communicates to the operator.
    pub async fn wire_clustering_claims(
        &self,
        claim_store: Arc<dyn ClaimStore>,
        node_identity: SharedNodeIdentity,
        durable_store: Option<Arc<dyn MucDurableStore>>,
        rollout_backoff: Option<Arc<dyn crate::ownership::RolloutBackoff>>,
    ) {
        if let Err(error) = self
            .inner
            .tell(WireClusteringClaims {
                claim_store,
                node_identity,
                durable_store,
                rollout_backoff,
            })
            .await
        {
            warn!(%error, "failed to wire clustering claims into the room registry");
        }
    }

    /// Test-only: route a never-returning message through the same instrumented
    /// path as the public methods, so the reply-timeout → typed-error mapping is
    /// exercised against real wrapper code rather than a duplicated stub.
    #[cfg(test)]
    pub(crate) async fn hang_forever(&self) -> Result<(), RoomRegistryError> {
        let started = Instant::now();
        let result = self
            .inner
            .ask(super::room_registry_actor::HangForever)
            .mailbox_timeout(ROOM_REGISTRY_MAILBOX_TIMEOUT)
            .reply_timeout(ROOM_REGISTRY_REPLY_TIMEOUT)
            .await;
        Self::classify("hang_forever", started.elapsed(), result)
    }
}

/// A short, stable reason label for [`metrics::record_actor_request_dropped`].
///
/// Only the transport-failure variants reach this helper: [`RoomRegistry::classify`]
/// matches `HandlerError` and `Timeout` before its `Err(other)` arm calls this,
/// so those map through the catch-all rather than dedicated arms (which would be
/// dead code from the sole call site).
fn send_error_reason<M, E>(error: &SendError<M, E>) -> &'static str {
    match error {
        SendError::ActorNotRunning(_) => "actor_not_running",
        SendError::ActorStopped => "actor_stopped",
        SendError::MailboxFull(_) => "mailbox_full",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests;
