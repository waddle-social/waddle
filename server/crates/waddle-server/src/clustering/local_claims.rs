//! Real `LocallyClaimedEntities` backed by the SM session registry
//! (ADR-0017 Phase 3 Slice 5, carried debt (b): `self_fence.rs`'s doc
//! comment on [`super::self_fence::NoLocallyClaimedEntities`] flagged this
//! as the production wiring a later slice must supply).
//!
//! **Construction-order note**: `clustering::start_if_enabled` must hand a
//! `Arc<dyn LocallyClaimedEntities>` to `self_fence::run_node_lease` before
//! the SM session registry exists — the registry itself is constructed
//! later, in `server/http.rs::create_sm_session_registry`, which needs
//! `ClusteringHandles` (returned by `start_if_enabled`) to obtain the
//! `ClaimStore`/live-identity pair it wires into the registry. This module
//! resolves the ordering with a fill-in-later cell: [`SmSessionLocalClaims::new`]
//! constructs an empty instance immediately (its `owned()` reports nothing
//! and `demote`/`health_check` are no-ops until wired — identical
//! observable behavior to [`super::self_fence::NoLocallyClaimedEntities`]
//! for the brief startup window before the registry exists), and
//! [`SmSessionLocalClaims::wire`] completes it once the registry is built.
//! [`SmSessionLocalClaims::wire_connection_registry`] separately supplies the
//! already-live connection registry so demotion can distinguish attached
//! streams and hand their termination to the owning WebSocket task.
//!
//! **Scope note**: this module reports SM-session, RoomActor, and UserActor
//! claims to the generic node-lease/self-fence machinery. UserActor local
//! claims use the same fill-in-later cell pattern as SM sessions: the
//! registry is created later in `server/http.rs`, then wired into the handle
//! that `start_if_enabled` already handed to `run_node_lease`.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use jid::{BareJid, FullJid};
use kameo::actor::ActorRef;
use waddle_xmpp::muc::room_actor::HealthCheck;
use waddle_xmpp::muc::RoomRegistry;
use waddle_xmpp::ownership::{Entity, EntityType};
use waddle_xmpp::registry::user_actor::HealthCheck as UserHealthCheck;
use waddle_xmpp::registry::{
    ConnectionRegistry, DemoteUserActor, DemoteUserActorIfOwner, DemotedUserResource,
    ForceDetachOutcome, ForceDetachRequest, GetResources, GetUserForLocalClaim, ListUsers,
    ListUsersOwnedBy, UserRegistryActor,
};
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;

use super::self_fence::{LocallyClaimedEntities, ReclaimedHydrationHandoff};

/// Bound on the health-ask this impl issues against a locally-claimed
/// room's `RoomActor` (ADR-0017 Phase 3 Slice 7's `RoomActor` counterpart
/// of the `UserActor` owner-veto path). Chosen well below the steal-intent
/// TTL / heartbeat interval so a genuinely wedged room is detected and
/// hard-killed within one veto-scan tick, never straddling two.
const ROOM_HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const USER_HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const USER_FORCE_DETACH_ACK_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug)]
enum LocalClaimsErrorClass {
    InvalidEntityId,
    RegistryUnavailable,
    MailboxUnavailable,
}

impl LocalClaimsErrorClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidEntityId => "invalid_entity_id",
            Self::RegistryUnavailable => "registry_unavailable",
            Self::MailboxUnavailable => "mailbox_unavailable",
        }
    }
}

impl std::fmt::Display for LocalClaimsErrorClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// See the module doc for the construction-order rationale.
pub struct SmSessionLocalClaims {
    registry: OnceLock<Arc<InMemorySmSessionRegistry>>,
    connection_registry: OnceLock<Arc<ConnectionRegistry>>,
}

impl SmSessionLocalClaims {
    /// Construct empty. See the module doc's construction-order note.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            registry: OnceLock::new(),
            connection_registry: OnceLock::new(),
        })
    }

    /// Wire in the real registry once it exists. A registry is constructed
    /// exactly once per process, so a second call is a programming error —
    /// logged rather than panicked, since the running node-lease loop must
    /// not crash over it.
    pub fn wire(&self, registry: Arc<InMemorySmSessionRegistry>) {
        if self.registry.set(registry).is_err() {
            tracing::error!(
                "SmSessionLocalClaims::wire called more than once; the SM session \
                 registry handle was already wired (ignoring this call)"
            );
        }
    }

    pub fn wire_connection_registry(&self, registry: Arc<ConnectionRegistry>) {
        if self.connection_registry.set(registry).is_err() {
            tracing::error!(
                "SmSessionLocalClaims::wire_connection_registry called more than once; the \
                 connection registry handle was already wired (ignoring this call)"
            );
        }
    }

    async fn demote_stream(&self, registry: &InMemorySmSessionRegistry, stream_id: &str) {
        // Authoritative local demotion FIRST, unconditionally. The
        // `LocallyClaimedEntities::demote` contract (self_fence.rs, FIX 3)
        // requires demotion to be purely local, cheap, and effective against
        // a wedged target: the un-deadlined lease-reconcile and terminal-
        // sweep loops call this once per lost entity and must never block on
        // a connection task. Mirrors `UserLocalClaims::demote`'s removal-
        // then-force-detach ordering. Durable writes stay safe during the
        // signal window below: demotion's premise is that the `ClaimStore`
        // row is gone or reassigned, so an in-flight fenced write fails
        // `NotOwner` at the database regardless of the in-memory fence.
        registry.forget_claim_locally(stream_id).await;

        // Then, best effort and without awaiting the connection: hand the
        // attached WebSocket (if any) its termination signal so inbound
        // processing stops instead of riding on without local authority. A
        // wedged or torn-down connection changes nothing above; the
        // acknowledgement is observed off-path for telemetry only. A later
        // clean detach re-ensures the claim idempotently and rebuilds
        // consistent custody if this node in fact still owns the entity.
        let attached = self.connection_registry.get().and_then(|connections| {
            let session_id = waddle_xmpp::pending_delivery::SmSessionId::new(stream_id);
            let jid = connections.sm_stream_owner(&session_id)?;
            let entry = connections.get_entry(&jid)?;
            (entry.sm_stream_id().as_ref() == Some(&session_id)).then_some((jid, entry))
        });
        let Some((jid, entry)) = attached else {
            return;
        };
        let (ack, ack_rx) = tokio::sync::oneshot::channel();
        let request = ForceDetachRequest {
            origin: waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement,
            requester_bare_jid: jid.to_bare(),
            ack,
        };
        // Delivery and acknowledgement both live in a detached, bounded,
        // span-instrumented task: `send().await` rides out a momentarily
        // full force-detach channel (a `try_send` here would silently drop
        // the only termination signal for a socket whose fence is already
        // gone), while `demote` itself stays purely local and returns
        // immediately. A closed channel or an expired budget means the
        // connection is already tearing down or wedged; the fence removal
        // above stands either way and cleanup owns the rest.
        let sender = entry.force_detach_sender();
        let observed_stream_id = stream_id.to_string();
        tokio::spawn(tracing::Instrument::instrument(
            async move {
                let delivery = tokio::time::timeout(USER_FORCE_DETACH_ACK_TIMEOUT, async {
                    sender.send(request).await.is_ok()
                })
                .await;
                match delivery {
                    Ok(true) => {}
                    Ok(false) | Err(_) => {
                        tracing::warn!(
                            stream_id = %observed_stream_id,
                            jid = %jid,
                            "demoted SM stream's connection could not be signalled for termination"
                        );
                        return;
                    }
                }
                match tokio::time::timeout(USER_FORCE_DETACH_ACK_TIMEOUT, ack_rx).await {
                    Ok(Ok(_outcome)) => {}
                    Ok(Err(_)) | Err(_) => {
                        tracing::warn!(
                            stream_id = %observed_stream_id,
                            jid = %jid,
                            "demoted SM stream's connection never acknowledged termination"
                        );
                    }
                }
            },
            tracing::Span::current(),
        ));
    }
}

#[async_trait]
impl LocallyClaimedEntities for SmSessionLocalClaims {
    async fn owned(&self) -> Vec<Entity> {
        let Some(registry) = self.registry.get() else {
            return Vec::new();
        };
        registry
            .locally_owned_claim_ids()
            .unwrap_or_default()
            .into_iter()
            .map(|stream_id| Entity::new(EntityType::SmSession, stream_id))
            .collect()
    }

    async fn demote(&self, entity: &Entity) {
        let Some(registry) = self.registry.get() else {
            return;
        };
        if entity.entity_type != EntityType::SmSession {
            return;
        }
        self.demote_stream(registry, &entity.id).await;
    }

    async fn demote_owned_by(&self, owner: &waddle_xmpp::ownership::NodeIdentity) {
        let Some(registry) = self.registry.get() else {
            return;
        };
        let entities = registry
            .locally_owned_claim_ids_for_owner(owner)
            .unwrap_or_default();
        for stream_id in entities {
            self.demote_stream(registry, &stream_id).await;
        }
    }

    async fn health_check(&self, _entity: &Entity) -> bool {
        // Structurally unreachable in production this slice (see the
        // module doc's scope note): `owned()` only ever reports
        // `SmSession` entities, and steal-intents never apply to those
        // (Slice 3 rule 1), so `owner_steal_intents` never returns one and
        // `run_node_lease`'s veto scan never calls this. Trivially healthy
        // rather than `todo!`/panic, mirroring
        // `NoLocallyClaimedEntities`'s identical precedent.
        true
    }

    fn reserve_reclaimed_claim_capacity(
        &self,
        entity: &Entity,
    ) -> Option<waddle_xmpp::stream_management::ReclaimedClaimReservation> {
        self.registry
            .get()
            .and_then(|registry| registry.reserve_reclaimed_claim_capacity(entity))
    }

    fn cancel_reclaimed_claim_capacity(
        &self,
        entity: &Entity,
        reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    ) {
        if let Some(registry) = self.registry.get() {
            registry.cancel_reclaimed_claim_capacity(entity, reservation);
        }
    }

    fn defer_uncertain_reclaimed_claim(
        &self,
        entity: &Entity,
        owner: &waddle_xmpp::ownership::NodeIdentity,
        reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    ) {
        if let Some(registry) = self.registry.get() {
            registry.defer_uncertain_reclaimed_claim(entity, owner, reservation);
        }
    }

    /// FIX 4(b) (ADR-0017 Phase 3 Slice 5 corrigenda): delegates straight
    /// to `InMemorySmSessionRegistry::hydrate_reclaimed` — the same
    /// targeted, per-entity-shard-locked hydration path the orphan reaper
    /// uses (FIX 2), so `self_fence::run_node_lease`'s inline post-fence
    /// reclaim and the general reaper share one hydration implementation.
    /// A no-op before `wire` runs, mirroring `demote`/`owned`'s identical
    /// unwired behavior.
    #[tracing::instrument(name = "clustering.sm_session.hydrate_reclaimed", skip_all)]
    async fn hydrate_reclaimed(
        &self,
        entities: &[(
            Entity,
            waddle_xmpp::ownership::NodeIdentity,
            waddle_xmpp::ownership::ClaimEpoch,
            waddle_xmpp::stream_management::ReclaimedClaimReservation,
        )],
    ) -> ReclaimedHydrationHandoff {
        let Some(registry) = self.registry.get() else {
            return ReclaimedHydrationHandoff::NotAccepted;
        };
        for (entity, owner, epoch, reservation) in entities {
            let fence = waddle_xmpp::stream_management::persistence::SmClaimFence::new(
                owner.clone(),
                *epoch,
            );
            match registry
                .hydrate_reclaimed_typed(entity, &fence, *reservation)
                .await
            {
                Ok(
                    waddle_xmpp::stream_management::ReclaimedHydrationOutcome::MissingDurable
                    | waddle_xmpp::stream_management::ReclaimedHydrationOutcome::PoisonReleased
                    | waddle_xmpp::stream_management::ReclaimedHydrationOutcome::StaleIdentity,
                ) => {
                    if let Err(_error) = registry
                        .release_reclaimed_claim(entity, &fence, *reservation)
                        .await
                    {
                        // Internal reclaim repair failure should stay visible for postmortem
                        // analysis: leave ownership in place and let retry logic reclaim later.
                        crate::telemetry::mark_span_error(
                            "sm_session_rehydrate: failed to release reclaimed claim",
                        );
                        tracing::warn!(
                            error_class = %LocalClaimsErrorClass::RegistryUnavailable,
                            entity_id = %entity.id,
                            "SmSessionLocalClaims::hydrate_reclaimed: terminal exact release failed; responsibility retained for retry"
                        );
                    }
                }
                Ok(_) => {}
                Err(_error) => {
                    // Rehydrate failed before ownership transfer: this is not protocol
                    // failure, but it should still export a failed span for incident
                    // detection and operator alerting.
                    crate::telemetry::mark_span_error(
                        "sm_session_rehydrate: failed to hydrate reclaimed claim",
                    );
                    tracing::warn!(
                        error_class = %LocalClaimsErrorClass::RegistryUnavailable,
                        entity_id = %entity.id,
                        "SmSessionLocalClaims::hydrate_reclaimed: registry hydrate_reclaimed failed"
                    );
                    return ReclaimedHydrationHandoff::NotAccepted;
                }
            }
        }
        ReclaimedHydrationHandoff::Accepted
    }
}

/// Real `LocallyClaimedEntities` backed by the MUC room registry (ADR-0017
/// Phase 3 Slice 7). Mirrors [`SmSessionLocalClaims`]'s exact
/// fill-in-later-cell shape: [`RoomLocalClaims::new`] constructs empty
/// (unwired `owned()`/`demote`/`health_check` are all safe no-ops/trivially
/// healthy, identical to this type's pre-wire behavior being
/// indistinguishable from [`super::self_fence::NoLocallyClaimedEntities`]),
/// and [`RoomLocalClaims::wire`] completes it once the room registry
/// exists.
///
/// Also the demote target for the Demote relay ask's receiving side
/// (ADR-0017 Phase 3 Slice 7's two-part demotion protocol, part (a)): the
/// same `Arc<RoomLocalClaims>` is threaded into [`super::relay::RelayActor`]
/// so a received `Demote` ask routes through the identical hard-kill
/// discipline [`LocallyClaimedEntities::demote`]'s doc contract requires.
pub struct RoomLocalClaims {
    registry: OnceLock<RoomRegistry>,
}

impl RoomLocalClaims {
    /// Construct empty. See the struct doc's construction-order note.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            registry: OnceLock::new(),
        })
    }

    /// Wire in the real room registry once it exists. A registry is
    /// constructed exactly once per process, so a second call is a
    /// programming error — logged rather than panicked, mirroring
    /// [`SmSessionLocalClaims::wire`].
    pub fn wire(&self, registry: RoomRegistry) {
        if self.registry.set(registry).is_err() {
            tracing::error!(
                "RoomLocalClaims::wire called more than once; the room \
                 registry handle was already wired (ignoring this call)"
            );
        }
    }

    /// Parse a `RoomActor` entity's id back into the room's bare JID.
    /// `None` (logged) for a malformed id — a data-integrity anomaly this
    /// impl never itself produces (every `Entity::new(EntityType::RoomActor,
    /// ..)` call site in the room registry binds `room_jid.to_string()`),
    /// so this only guards against a foreign/corrupted row.
    fn room_jid(entity: &Entity) -> Option<BareJid> {
        if entity.entity_type != EntityType::RoomActor {
            return None;
        }
        match entity.id.parse::<BareJid>() {
            Ok(jid) => Some(jid),
            Err(_error) => {
                tracing::warn!(
                    error_class = %LocalClaimsErrorClass::InvalidEntityId,
                    id = %entity.id,
                    "RoomLocalClaims: entity id is not a valid room JID"
                );
                None
            }
        }
    }
}

#[async_trait]
impl LocallyClaimedEntities for RoomLocalClaims {
    async fn owned(&self) -> Vec<Entity> {
        let Some(registry) = self.registry.get() else {
            return Vec::new();
        };
        // `owned()` is documented as "local bookkeeping only, never a
        // Postgres read" — `list_rooms()` is exactly that: an in-memory
        // enumeration of this process's live `RoomActor`s (an actor ask,
        // hence `async`, but no Postgres round-trip). Every
        // locally-spawned room always holds this node's claim by
        // construction (the registry's `GetOrCreateRoom`/`CreateRoom`/
        // `CreateInstantRoom` acquire the claim before spawning), so this
        // enumeration is exactly the owned-entity set.
        match registry.list_rooms().await {
            Ok(mut jids) => {
                if let Ok(pending) = registry.list_pending_room_release_jids().await {
                    jids.extend(pending);
                    jids.sort();
                    jids.dedup();
                }
                jids.into_iter()
                    .map(|jid| Entity::new(EntityType::RoomActor, jid.to_string()))
                    .collect()
            }
            Err(_error) => {
                tracing::warn!(
                    error_class = %LocalClaimsErrorClass::RegistryUnavailable,
                    "RoomLocalClaims::owned: room registry list_rooms failed; \
                     reporting no owned rooms this interval (fail-safe: a \
                     wedged/unreachable registry cannot report ownership it \
                     cannot itself observe)"
                );
                Vec::new()
            }
        }
    }

    /// FIX 3-equivalent hard-kill (ADR-0017 Phase 3 Slice 7): `RoomActor`
    /// holds no live connection/socket resources of its own (occupancy is
    /// tracked by full JID/nick; actual socket delivery is owned by
    /// `UserActor`/`ConnectionRegistry`), so unlike
    /// `UserActor::health_check_or_wedge_kill` there is no
    /// `ConflictCloseAllResources`-equivalent teardown step needed —
    /// `ActorRef::kill()` alone drops the actor's entire state
    /// (occupants, config, subject, affiliation cache) regardless of
    /// whether its mailbox loop is otherwise wedged, satisfying the same
    /// "effective against a wedged target" contract
    /// [`LocallyClaimedEntities::demote`]'s doc comment requires. The
    /// registry's own dead-actor detection (`live_room`) self-heals the
    /// registry map on the next access — no separate eviction call is
    /// needed here.
    async fn demote(&self, entity: &Entity) {
        let Some(registry) = self.registry.get() else {
            return;
        };
        let Some(room_jid) = Self::room_jid(entity) else {
            return;
        };
        if let Ok(Some(actor_ref)) = registry.get_room(room_jid.clone()).await {
            actor_ref.kill();
            tracing::warn!(
                room = %room_jid,
                "demoted (hard-killed) a locally-claimed RoomActor: Postgres \
                 no longer attributes this room's claim to this node"
            );
        }
    }

    async fn demote_owned_by(&self, owner: &waddle_xmpp::ownership::NodeIdentity) {
        let Some(registry) = self.registry.get() else {
            return;
        };
        let Ok(room_jids) = registry.list_rooms_owned_by(owner.clone()).await else {
            return;
        };
        for room_jid in room_jids {
            if registry
                .demote_room_if_owner(room_jid.clone(), owner.clone())
                .await
                .unwrap_or(false)
            {
                tracing::warn!(
                    room = %room_jid,
                    owner = %owner.node_id,
                    "post-rotation exact-owner sweep demoted stale RoomActor"
                );
            }
        }
    }

    /// Health-ask the local `RoomActor` (ADR-0017 Phase 3 Slice 3's
    /// owner-veto path, now genuinely applicable to `RoomActor` claims).
    ///
    /// ADR-0017 Phase 3 Slice 7 FIX 3 (council-adjudicated): "this node
    /// holds the claim but no live local actor exists" is UNHEALTHY,
    /// never healthy — a wedged/poisoned/gone room must not veto another
    /// node's legitimate steal-intent. Both `Ok(None)` (the registry has
    /// no live actor for this room — e.g. a residual window before
    /// `live_room`'s own dead-actor release completes, or this process
    /// simply never spawned it) and `Err(_)` (the registry ask itself
    /// failed, or the room is mid-`RoomActorStateLost`) are treated as
    /// unhealthy. Previously both were reported healthy ("nothing to
    /// report unhealthy about"), which meant an orphaned claim's owner
    /// -veto entry could indefinitely block a legitimate steal-intent
    /// from another node for a room this process can no longer act on at
    /// all.
    async fn health_check(&self, entity: &Entity) -> bool {
        let Some(registry) = self.registry.get() else {
            return true;
        };
        let Some(room_jid) = Self::room_jid(entity) else {
            return true;
        };
        if registry
            .is_current_room_pending_release(room_jid.clone())
            .await
            .unwrap_or(false)
        {
            return false;
        }
        let Ok(Some(actor_ref)) = registry.get_room(room_jid).await else {
            tracing::warn!(
                %entity,
                "RoomLocalClaims::health_check: this node's claim has no live local \
                 actor to ask; reporting UNHEALTHY so the veto scan can demote/release \
                 rather than blocking a legitimate steal-intent"
            );
            return false;
        };
        actor_ref
            .ask(HealthCheck)
            .mailbox_timeout(ROOM_HEALTH_CHECK_TIMEOUT)
            .reply_timeout(ROOM_HEALTH_CHECK_TIMEOUT)
            .await
            .is_ok()
    }

    /// ADR-0017 Phase 3 Slice 10 FIX 1 (council-adjudicated): the seal
    /// barrier element 4's drain sequence requires before an owned room
    /// enters `run_shutdown_drain`'s release batch — landed here for real,
    /// rather than falling through to the trait's trivial `true` default
    /// (which would batch every owned room for release **instantly**, with
    /// no confirmation that a mutation already queued ahead of the drain
    /// snapshot has finished its final fenced write).
    ///
    /// Issues the SAME `HealthCheck` ask [`Self::health_check`] above uses,
    /// as a genuine mailbox-ordering barrier: kameo serializes each actor's
    /// mailbox strictly in order (see [`HealthCheck`]'s own doc comment),
    /// and every mutation handler this actor exposes
    /// (`UpdateConfig`/`RollbackConfigIfRevision`/
    /// `UpdateGroupDmConfigByMember`/`SetSubject`/`ChangeAffiliation`, and
    /// the affiliation-bulk-apply path) synchronously `.await`s its
    /// ownership check and durable persist call
    /// (`persist_config`/`persist_subject`/`persist_affiliation`) before
    /// returning a reply — so a mutation enqueued ahead of this ask has
    /// already run its handler to completion, including its durable write's
    /// commit, by the time this ask's own reply lands. A room with no live
    /// local actor to ask, or one whose ask fails or times out (wedged),
    /// reports unsealed (`false`): [`crate::clustering::drain::
    /// run_shutdown_drain`] then leaves that claim held —
    /// `claims_abandoned_on_drain`, fenced-safe, reclaimed later — rather
    /// than releasing a claim whose final state it could not confirm.
    async fn seal_before_release(&self, entity: &Entity) -> bool {
        let Some(registry) = self.registry.get() else {
            return true;
        };
        let Some(room_jid) = Self::room_jid(entity) else {
            return true;
        };
        if registry
            .is_current_identity_pending_room_release_only(room_jid.clone())
            .await
            .unwrap_or(false)
        {
            return true;
        }
        let Ok(Some(actor_ref)) = registry.get_room(room_jid).await else {
            tracing::warn!(
                %entity,
                "RoomLocalClaims::seal_before_release: this node's claim has no live \
                 local actor to seal; reporting unsealed so the drain leaves the claim \
                 held rather than releasing state it could not confirm"
            );
            return false;
        };
        actor_ref
            .ask(HealthCheck)
            .mailbox_timeout(ROOM_HEALTH_CHECK_TIMEOUT)
            .reply_timeout(ROOM_HEALTH_CHECK_TIMEOUT)
            .await
            .is_ok()
    }
}

/// Real `LocallyClaimedEntities` backed by the UserActor registry (ADR-0017
/// Phase 4 Slice 1b). Mirrors [`SmSessionLocalClaims`]'s fill-in-later-cell
/// shape because the user registry is spawned later, while building
/// `WebSocketState`.
pub struct UserLocalClaims {
    registry: OnceLock<ActorRef<UserRegistryActor>>,
    connection_registry: OnceLock<Arc<ConnectionRegistry>>,
}

impl UserLocalClaims {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            registry: OnceLock::new(),
            connection_registry: OnceLock::new(),
        })
    }

    pub fn wire(&self, registry: ActorRef<UserRegistryActor>) {
        if self.registry.set(registry).is_err() {
            tracing::error!(
                "UserLocalClaims::wire called more than once; the user registry \
                 handle was already wired (ignoring this call)"
            );
        }
    }

    pub fn wire_connection_registry(&self, registry: Arc<ConnectionRegistry>) {
        if self.connection_registry.set(registry).is_err() {
            tracing::error!(
                "UserLocalClaims::wire_connection_registry called more than once; the \
                 connection registry handle was already wired (ignoring this call)"
            );
        }
    }

    fn user_jid(entity: &Entity) -> Option<BareJid> {
        if entity.entity_type != EntityType::UserActor {
            return None;
        }
        match entity.id.parse::<BareJid>() {
            Ok(jid) => Some(jid),
            Err(_error) => {
                tracing::warn!(
                    error_class = %LocalClaimsErrorClass::InvalidEntityId,
                    id = %entity.id,
                    "UserLocalClaims: entity id is not a valid user bare JID"
                );
                None
            }
        }
    }

    async fn actor_resources(
        registry: &ActorRef<UserRegistryActor>,
        bare_jid: &BareJid,
    ) -> Vec<FullJid> {
        let actor_ref = match registry
            .ask(GetUserForLocalClaim {
                bare_jid: bare_jid.clone(),
            })
            .mailbox_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .reply_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .await
        {
            Ok(Some(actor_ref)) => actor_ref,
            Ok(None) => return Vec::new(),
            Err(_error) => {
                // Missing actor handle after lookup means this claim is no longer
                // actively represented; demotion continues via best-effort cleanup,
                // but the span must still record the dependency failure.
                crate::telemetry::mark_span_error(
                    "user_local_claims: user registry lookup failed before force-detach",
                );
                tracing::warn!(
                    error_class = %LocalClaimsErrorClass::RegistryUnavailable,
                    jid = %bare_jid,
                    "UserLocalClaims::demote: user registry lookup failed before force-detach"
                );
                return Vec::new();
            }
        };
        match actor_ref
            .ask(GetResources)
            .mailbox_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .reply_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .await
        {
            Ok(resources) => resources,
            Err(_error) => {
                // Resource enumeration requires a live actor for force-detach;
                // failure here means an internal path diverged from best-effort
                // cleanup assumptions.
                crate::telemetry::mark_span_error(
                    "user_local_claims: failed to enumerate user actor resources",
                );
                tracing::warn!(
                    error_class = %LocalClaimsErrorClass::RegistryUnavailable,
                    jid = %bare_jid,
                    "UserLocalClaims::demote: UserActor resource enumeration failed before force-detach"
                );
                Vec::new()
            }
        }
    }

    fn connection_registry_resources(
        connection_registry: &ConnectionRegistry,
        bare_jid: &BareJid,
    ) -> Vec<FullJid> {
        connection_registry
            .list_connections()
            .into_iter()
            .filter(|jid| jid.to_bare() == *bare_jid)
            .collect()
    }

    fn merge_resources(resources: &mut Vec<FullJid>, extra: Vec<FullJid>) {
        for jid in extra {
            if !resources.contains(&jid) {
                resources.push(jid);
            }
        }
    }

    async fn force_detach_resources(&self, bare_jid: &BareJid, resources: Vec<FullJid>) {
        if resources.is_empty() {
            return;
        }
        let Some(connection_registry) = self.connection_registry.get() else {
            tracing::warn!(
                jid = %bare_jid,
                resource_count = resources.len(),
                "UserLocalClaims::demote: no ConnectionRegistry wired; cannot force-detach live resources"
            );
            // Without ConnectionRegistry we cannot complete demotion cleanup for live
            // connections on this process; this is an internal miswire/internal
            // dependency failure.
            crate::telemetry::mark_span_error(
                "user_local_claims: no connection registry available for force-detach",
            );
            return;
        };
        let entries = resources
            .into_iter()
            .filter_map(|jid| {
                connection_registry
                    .get_entry(&jid)
                    .map(|entry| (jid, entry))
            })
            .collect();
        self.force_detach_entries(bare_jid, entries).await;
    }

    async fn force_detach_exact_resources(
        &self,
        bare_jid: &BareJid,
        resources: Vec<DemotedUserResource>,
    ) {
        self.force_detach_entries(
            bare_jid,
            resources
                .into_iter()
                .map(|resource| (resource.jid, resource.entry))
                .collect(),
        )
        .await;
    }

    async fn force_detach_entries(
        &self,
        bare_jid: &BareJid,
        entries: Vec<(FullJid, waddle_xmpp::registry::ConnectionEntry)>,
    ) {
        let Some(connection_registry) = self.connection_registry.get() else {
            return;
        };
        for (jid, entry) in entries {
            let owner = entry.carbons_handle();
            let (ack, ack_rx) = tokio::sync::oneshot::channel();
            let request = ForceDetachRequest {
                origin: waddle_xmpp::registry::ForceDetachOrigin::OwnerManagedRetirement,
                requester_bare_jid: bare_jid.clone(),
                ack,
            };
            let mut remove_after_wait = false;
            match entry.force_detach_sender().try_send(request) {
                Ok(()) => match tokio::time::timeout(USER_FORCE_DETACH_ACK_TIMEOUT, ack_rx).await {
                    Ok(Ok(ForceDetachOutcome::Detached | ForceDetachOutcome::NotPersisted)) => {
                        remove_after_wait = true;
                        tracing::debug!(
                            jid = %jid,
                            "UserLocalClaims::demote: connection task acknowledged force-detach"
                        );
                    }
                    Ok(Ok(ForceDetachOutcome::IdentityMismatch)) => {
                        remove_after_wait = false;
                        tracing::warn!(
                            jid = %jid,
                            requester = %bare_jid,
                            "UserLocalClaims::demote: force-detach identity mismatch; leaving registry entry untouched"
                        );
                        // Identity changed while detach was in flight; keep failure
                        // visibility in traces for later reconciliation debugging.
                        crate::telemetry::mark_span_error(
                            "user_local_claims: force-detach identity mismatch",
                        );
                    }
                    Ok(Err(_closed)) => {
                        tracing::warn!(
                            jid = %jid,
                            "UserLocalClaims::demote: force-detach ack channel closed before response; leaving registry entry for connection-owned cleanup"
                        );
                        // Ack channel closure is an internal detach-path failure even
                        // if cleanup can continue asynchronously.
                        crate::telemetry::mark_span_error(
                            "user_local_claims: force-detach ack channel closed",
                        );
                    }
                    Err(_elapsed) => {
                        tracing::warn!(
                            jid = %jid,
                            timeout_ms = USER_FORCE_DETACH_ACK_TIMEOUT.as_millis() as u64,
                            "UserLocalClaims::demote: force-detach timed out; leaving registry entry so the connection task does not misclassify cleanup as superseded"
                        );
                        // Timeout indicates stalled force-detach coordination; mark span
                        // failed to keep this in incident triage, even though it remains
                        // best-effort.
                        crate::telemetry::mark_span_error(
                            "user_local_claims: force-detach timed out",
                        );
                    }
                },
                Err(_error) => {
                    tracing::warn!(
                        error_class = %LocalClaimsErrorClass::MailboxUnavailable,
                        jid = %jid,
                        "UserLocalClaims::demote: force-detach request could not be queued; leaving registry entry for connection-owned cleanup"
                    );
                    // If the detach request cannot be queued, we intentionally defer cleanup,
                    // but this is still an internal failure and should become a span error.
                    crate::telemetry::mark_span_error(
                        "user_local_claims: force-detach request could not be queued",
                    );
                }
            }
            if remove_after_wait
                && connection_registry
                    .unregister_if_owner(&jid, &owner)
                    .is_some()
            {
                tracing::warn!(
                    jid = %jid,
                    "UserLocalClaims::demote: removed deposed resource from ConnectionRegistry"
                );
            }
        }
    }
}

#[async_trait]
impl LocallyClaimedEntities for UserLocalClaims {
    async fn owned(&self) -> Vec<Entity> {
        // Authoritative enumeration ONLY — this feeds the per-tick lease
        // reconcile, which demotes every entity this node does not own.
        // The user registry's mirror entries are the local-claim source of
        // truth: every locally OWNED user has one, while a connection whose
        // bare-JID UserActor claim belongs to another node (a same-bare
        // remote-hosted resource) deliberately has none. Enumerating raw
        // ConnectionRegistry sockets here made every reconcile tick demote
        // such foreign-owned connections and force-detach the healthy live
        // socket with <stream:error><conflict/> (#1680) — and a transient
        // list failure must not resurrect that bug, so on failure this
        // reports NOTHING (the reconcile pass skips users this tick and
        // re-converges on the next one) instead of falling back to the
        // transport sweep. The broad sweep lives in
        // [`LocallyClaimedEntities::terminal_sweep`].
        let Some(registry) = self.registry.get() else {
            return Vec::new();
        };
        match registry
            .ask(ListUsers)
            .mailbox_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .reply_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .await
        {
            Ok(jids) => jids
                .into_iter()
                .map(|jid| Entity::new(EntityType::UserActor, jid.to_string()))
                .collect(),
            Err(_error) => {
                tracing::warn!(
                    error_class = %LocalClaimsErrorClass::RegistryUnavailable,
                    "UserLocalClaims::owned: user registry list_users failed; \
                     skipping user reconcile this tick (no transport fallback: #1680)"
                );
                Vec::new()
            }
        }
    }

    async fn terminal_sweep(&self) -> Vec<Entity> {
        // Terminal teardown: the node is ceasing to serve, so EVERY local
        // resource must be swept — including raw ConnectionRegistry sockets
        // the user registry cannot enumerate (its listing failed, it is
        // unwired, or the resource is a remote-hosted mirror-less
        // connection). Force-detaching foreign-owned sockets is correct
        // here: their transport is dying with the node either way.
        let mut owned = self.owned().await;
        if let Some(connection_registry) = self.connection_registry.get() {
            for jid in connection_registry.list_connections() {
                let entity = Entity::new(EntityType::UserActor, jid.to_bare().to_string());
                if !owned.contains(&entity) {
                    owned.push(entity);
                }
            }
        }
        owned
    }

    #[tracing::instrument(name = "clustering.user_actor.demote", skip_all)]
    async fn demote(&self, entity: &Entity) {
        let Some(registry) = self.registry.get() else {
            return;
        };
        let Some(bare_jid) = Self::user_jid(entity) else {
            return;
        };
        let mut resources = Self::actor_resources(registry, &bare_jid).await;
        if let Some(connection_registry) = self.connection_registry.get() {
            Self::merge_resources(
                &mut resources,
                Self::connection_registry_resources(connection_registry, &bare_jid),
            );
        }
        match registry
            .ask(DemoteUserActor {
                bare_jid: bare_jid.clone(),
            })
            .mailbox_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .reply_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .await
        {
            Ok(true) => {
                tracing::warn!(
                    jid = %bare_jid,
                    "demoted (hard-killed) a locally-claimed UserActor: Postgres \
                     no longer attributes this user claim to this node"
                );
            }
            Ok(false) => {}
            Err(_error) => {
                // UserActor demotion failed in local-claim cleanup; this is an internal
                // operation failure and should remain queryable by span status.
                crate::telemetry::mark_span_error("user_local_claims: user actor demotion failed");
                tracing::warn!(
                    error_class = %LocalClaimsErrorClass::RegistryUnavailable,
                    jid = %bare_jid,
                    "UserLocalClaims::demote: user registry demotion failed"
                );
            }
        }
        self.force_detach_resources(&bare_jid, resources).await;
    }

    async fn health_check(&self, entity: &Entity) -> bool {
        let Some(registry) = self.registry.get() else {
            return false;
        };
        let Some(bare_jid) = Self::user_jid(entity) else {
            return false;
        };
        let actor_ref = match registry
            .ask(GetUserForLocalClaim {
                bare_jid: bare_jid.clone(),
            })
            .mailbox_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .reply_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .await
        {
            Ok(Some(actor_ref)) => actor_ref,
            Ok(None) => {
                tracing::warn!(
                    jid = %bare_jid,
                    "UserLocalClaims::health_check: this node's claim has no live \
                     local UserActor; reporting UNHEALTHY"
                );
                return false;
            }
            Err(_error) => {
                tracing::warn!(
                    error_class = %LocalClaimsErrorClass::RegistryUnavailable,
                    jid = %bare_jid,
                    "UserLocalClaims::health_check: user registry lookup failed; \
                     reporting UNHEALTHY"
                );
                return false;
            }
        };
        actor_ref
            .ask(UserHealthCheck)
            .mailbox_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .reply_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .await
            .is_ok()
    }

    async fn seal_before_release(&self, entity: &Entity) -> bool {
        if entity.entity_type == EntityType::UserActor {
            tracing::warn!(
                %entity,
                "UserLocalClaims::seal_before_release: UserActor has no final durable \
                 seal barrier; leaving claim held for fenced reclaim"
            );
            false
        } else {
            true
        }
    }

    #[tracing::instrument(name = "clustering.user_actor.demote_owned_by", skip_all)]
    async fn demote_owned_by(&self, owner: &waddle_xmpp::ownership::NodeIdentity) {
        let Some(registry) = self.registry.get() else {
            return;
        };
        let users = match registry
            .ask(ListUsersOwnedBy {
                owner: owner.clone(),
            })
            .mailbox_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .reply_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .await
        {
            Ok(users) => users,
            Err(_error) => {
                tracing::warn!(
                    error_class = %LocalClaimsErrorClass::RegistryUnavailable,
                    "UserLocalClaims exact-owner listing failed"
                );
                return;
            }
        };
        for bare_jid in users {
            match registry
                .ask(DemoteUserActorIfOwner {
                    bare_jid: bare_jid.clone(),
                    owner: owner.clone(),
                })
                .mailbox_timeout(USER_HEALTH_CHECK_TIMEOUT)
                .reply_timeout(USER_HEALTH_CHECK_TIMEOUT)
                .await
            {
                Ok(Some(demoted)) => {
                    self.force_detach_exact_resources(&bare_jid, demoted.resources)
                        .await;
                    tracing::warn!(
                        jid = %bare_jid,
                        owner = %owner.node_id,
                        "post-rotation exact-owner sweep demoted stale UserActor"
                    );
                }
                Ok(None) => {}
                Err(_error) => {
                    // Exact-owner demotion failed after ownership check; this is an
                    // internal consistency error during cluster recovery.
                    crate::telemetry::mark_span_error(
                        "user_local_claims: exact-owner demotion failed",
                    );
                    tracing::warn!(
                        error_class = %LocalClaimsErrorClass::RegistryUnavailable,
                        jid = %bare_jid,
                        "exact-owner UserActor demotion failed"
                    );
                }
            }
        }
    }
}

/// Dispatches [`LocallyClaimedEntities`] calls across `SmSession`,
/// `RoomActor`, and `UserActor` claims by `entity.entity_type`:
/// `run_node_lease` takes exactly one `Arc<dyn LocallyClaimedEntities>`
/// handle, but Slices 5 and 7 each contribute their own concrete
/// implementation over a different owned-entity universe. Combining them
/// here, rather than widening any concrete type to know about the
/// other's entities, keeps `SmSessionLocalClaims`/`RoomLocalClaims`/
/// `UserLocalClaims`
/// independently testable (as both already are).
pub struct CombinedLocalClaims {
    sm: Arc<SmSessionLocalClaims>,
    room: Arc<RoomLocalClaims>,
    user: Arc<UserLocalClaims>,
}

impl CombinedLocalClaims {
    pub fn new(
        sm: Arc<SmSessionLocalClaims>,
        room: Arc<RoomLocalClaims>,
        user: Arc<UserLocalClaims>,
    ) -> Arc<Self> {
        Arc::new(Self { sm, room, user })
    }
}

#[async_trait]
impl LocallyClaimedEntities for CombinedLocalClaims {
    async fn owned(&self) -> Vec<Entity> {
        let mut owned = self.sm.owned().await;
        owned.extend(self.room.owned().await);
        owned.extend(self.user.owned().await);
        owned
    }

    async fn terminal_sweep(&self) -> Vec<Entity> {
        let mut swept = self.sm.terminal_sweep().await;
        swept.extend(self.room.terminal_sweep().await);
        swept.extend(self.user.terminal_sweep().await);
        swept
    }

    async fn demote(&self, entity: &Entity) {
        match entity.entity_type {
            EntityType::SmSession => self.sm.demote(entity).await,
            EntityType::RoomActor => self.room.demote(entity).await,
            EntityType::UserActor => self.user.demote(entity).await,
        }
    }

    async fn demote_owned_by(&self, owner: &waddle_xmpp::ownership::NodeIdentity) {
        self.sm.demote_owned_by(owner).await;
        self.room.demote_owned_by(owner).await;
        self.user.demote_owned_by(owner).await;
    }

    async fn health_check(&self, entity: &Entity) -> bool {
        match entity.entity_type {
            EntityType::SmSession => self.sm.health_check(entity).await,
            EntityType::RoomActor => self.room.health_check(entity).await,
            EntityType::UserActor => self.user.health_check(entity).await,
        }
    }

    fn reserve_reclaimed_claim_capacity(
        &self,
        entity: &Entity,
    ) -> Option<waddle_xmpp::stream_management::ReclaimedClaimReservation> {
        match entity.entity_type {
            EntityType::SmSession => self.sm.reserve_reclaimed_claim_capacity(entity),
            EntityType::RoomActor | EntityType::UserActor => None,
        }
    }

    fn cancel_reclaimed_claim_capacity(
        &self,
        entity: &Entity,
        reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    ) {
        if entity.entity_type == EntityType::SmSession {
            self.sm.cancel_reclaimed_claim_capacity(entity, reservation);
        }
    }

    fn defer_uncertain_reclaimed_claim(
        &self,
        entity: &Entity,
        owner: &waddle_xmpp::ownership::NodeIdentity,
        reservation: waddle_xmpp::stream_management::ReclaimedClaimReservation,
    ) {
        if entity.entity_type == EntityType::SmSession {
            self.sm
                .defer_uncertain_reclaimed_claim(entity, owner, reservation);
        }
    }

    async fn hydrate_reclaimed(
        &self,
        entities: &[(
            Entity,
            waddle_xmpp::ownership::NodeIdentity,
            waddle_xmpp::ownership::ClaimEpoch,
            waddle_xmpp::stream_management::ReclaimedClaimReservation,
        )],
    ) -> ReclaimedHydrationHandoff {
        let (sm_entities, rest): (Vec<_>, Vec<_>) = entities
            .iter()
            .cloned()
            .partition(|(entity, _, _, _)| entity.entity_type == EntityType::SmSession);
        let sm_handoff = if sm_entities.is_empty() {
            ReclaimedHydrationHandoff::NotAccepted
        } else {
            self.sm.hydrate_reclaimed(&sm_entities).await
        };
        // `RoomLocalClaims`/`UserLocalClaims` have no reclaim-hydration
        // consumer yet (they are created through their registries' own
        // `ensure_claimed`/`steal_stale` paths), so `rest` is intentionally
        // not forwarded anywhere.
        if rest.is_empty() {
            sm_handoff
        } else {
            ReclaimedHydrationHandoff::NotAccepted
        }
    }

    /// ADR-0017 Phase 3 Slice 10 FIX 1: dispatch by `EntityType`, mirroring
    /// [`Self::demote`]/[`Self::health_check`] above. `SmSession` keeps the
    /// trait's trivial `true` default — the existing Q6 SM drain
    /// (`session_janitors::spawn_graceful_shutdown_drain`) already owns
    /// "final write, then release" for those entities on its own
    /// independent task (see `clustering::drain`'s module doc), so this
    /// generic seal is never consulted for `SmSession` at all
    /// (`clustering::drain::run_shutdown_drain` only ever batches
    /// `RoomActor` entities). `RoomActor` routes to
    /// [`RoomLocalClaims::seal_before_release`]'s real barrier. `UserActor`
    /// routes to [`UserLocalClaims::seal_before_release`], which currently
    /// fails closed because UserActor has no durable final-write barrier to
    /// prove before a generic drain release.
    async fn seal_before_release(&self, entity: &Entity) -> bool {
        match entity.entity_type {
            EntityType::SmSession => true,
            EntityType::RoomActor => self.room.seal_before_release(entity).await,
            EntityType::UserActor => self.user.seal_before_release(entity).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kameo::actor::Spawn;
    use waddle_xmpp::registry::ForceDetachOrigin;
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry as _};

    fn test_session(stream_id: &str, jid: &str) -> DetachedSession {
        DetachedSession {
            stream_id: stream_id.to_string(),
            user_id: jid.to_string(),
            jid: jid.parse().expect("valid jid"),
            inbound_count: 0,
            shadow_ordinal: waddle_xmpp::stream_management::ShadowOrdinal::ZERO,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: None,
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        }
    }

    #[tokio::test]
    async fn unwired_instance_owns_nothing() {
        let local_claims = SmSessionLocalClaims::new();
        assert!(local_claims.owned().await.is_empty());
    }

    #[tokio::test]
    async fn inline_reclaim_releases_claim_when_durable_session_is_missing() {
        use waddle_xmpp::ownership::{ClaimStore as _, InProcessClaimStore, NodeIdentity};

        let local_claims = SmSessionLocalClaims::new();
        let claim_store = Arc::new(InProcessClaimStore::new());
        let identity = NodeIdentity::new("self-fence-node", "fresh-incarnation");
        let entity = Entity::new(EntityType::SmSession, "missing-inline-reclaim");
        let epoch = claim_store
            .acquire(&entity, &identity)
            .await
            .expect("won inline reclaim epoch");
        let registry = Arc::new(InMemorySmSessionRegistry::new().with_claim_store(
            claim_store.clone(),
            waddle_xmpp::ownership::SharedNodeIdentity::new(identity.clone()),
        ));
        local_claims.wire(registry.clone());
        let reservation = registry
            .reserve_reclaimed_claim_capacity(&entity)
            .expect("inline reclaim reservation");

        local_claims
            .hydrate_reclaimed(&[(entity.clone(), identity, epoch, reservation)])
            .await;

        assert!(claim_store
            .current_claim(&entity)
            .await
            .expect("claim lookup")
            .is_none(), "MissingDurable must exact-release the inline self-fence claim instead of dropping the typed terminal outcome");
    }

    #[tokio::test]
    async fn inline_reclaim_exact_releases_a_genuinely_stale_identity() {
        use waddle_xmpp::ownership::{ClaimStore as _, InProcessClaimStore, NodeIdentity};

        let local_claims = SmSessionLocalClaims::new();
        let claim_store = Arc::new(InProcessClaimStore::new());
        let won_identity = NodeIdentity::new("self-fence-node", "won-incarnation");
        let current_identity = NodeIdentity::new("self-fence-node", "rotated-again");
        let entity = Entity::new(EntityType::SmSession, "stale-inline-reclaim");
        let epoch = claim_store
            .acquire(&entity, &won_identity)
            .await
            .expect("won inline reclaim epoch");
        let registry = Arc::new(InMemorySmSessionRegistry::new().with_claim_store(
            claim_store.clone(),
            waddle_xmpp::ownership::SharedNodeIdentity::new(current_identity),
        ));
        local_claims.wire(registry.clone());
        let reservation = registry
            .reserve_reclaimed_claim_capacity(&entity)
            .expect("inline reclaim reservation");

        local_claims
            .hydrate_reclaimed(&[(entity.clone(), won_identity, epoch, reservation)])
            .await;

        assert!(
            claim_store
                .current_claim(&entity)
                .await
                .expect("claim lookup")
                .is_none(),
            "a second identity rotation must exact-release the now-stale won generation instead of retaining it indefinitely"
        );
    }

    #[tokio::test]
    async fn owned_reflects_the_wired_registry_claim_inventory() {
        let local_claims = SmSessionLocalClaims::new();
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        registry
            .store_session(test_session("stream-a", "alice@example.com/res"))
            .await
            .expect("store session");
        local_claims.wire(Arc::clone(&registry));

        let owned = local_claims.owned().await;
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].entity_type, EntityType::SmSession);
        assert_eq!(owned[0].id, "stream-a");
    }

    #[tokio::test]
    async fn demote_forgets_the_claim_locally() {
        let local_claims = SmSessionLocalClaims::new();
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        registry
            .store_session(test_session("stream-b", "bob@example.com/res"))
            .await
            .expect("store session");
        local_claims.wire(Arc::clone(&registry));
        assert_eq!(local_claims.owned().await.len(), 1);

        let entity = Entity::new(EntityType::SmSession, "stream-b".to_string());
        local_claims.demote(&entity).await;
        assert!(local_claims.owned().await.is_empty());

        // A different entity type for the same id must be a no-op.
        registry
            .store_session(test_session("stream-c", "carol@example.com/res"))
            .await
            .expect("store session");
        let foreign_type_entity = Entity::new(EntityType::UserActor, "stream-c".to_string());
        local_claims.demote(&foreign_type_entity).await;
        assert_eq!(
            local_claims.owned().await.len(),
            1,
            "demote must not touch an entity of a different EntityType sharing the same id"
        );
    }

    #[tokio::test]
    async fn attached_sm_demotion_forgets_locally_first_and_signals_termination() {
        let local_claims = SmSessionLocalClaims::new();
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        let connections = Arc::new(ConnectionRegistry::new());
        let stream_id = "attached-demotion";
        let publication = registry
            .ensure_session_claim(stream_id)
            .await
            .expect("attached claim");
        drop(publication);
        local_claims.wire(Arc::clone(&registry));
        local_claims.wire_connection_registry(Arc::clone(&connections));

        let jid: FullJid = "alice@example.com/attached-demotion".parse().expect("jid");
        let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel(1);
        let owner = connections.register(jid.clone(), outbound_tx);
        assert!(connections.set_sm_stream_id_if_owner(
            &jid,
            &owner,
            Some(waddle_xmpp::pending_delivery::SmSessionId::new(stream_id)),
        ));
        let entry = connections
            .entry_if_owner(&jid, &owner)
            .expect("attached entry");
        let mut force_detach_rx = entry
            .take_force_detach_rx()
            .expect("connection owns force-detach receiver");

        local_claims
            .demote(&Entity::new(EntityType::SmSession, stream_id))
            .await;

        // The local demotion is unconditional and complete before any
        // connection acknowledgement: `demote` must never block on (or be
        // gated by) the attached task (self_fence.rs demote contract FIX 3).
        assert_eq!(registry.current_sm_claim_fence(stream_id), None);
        assert!(local_claims.owned().await.is_empty());

        let request = force_detach_rx.recv().await.expect("termination request");
        assert_eq!(request.origin, ForceDetachOrigin::OwnerManagedRetirement);
        let _ = request.ack.send(ForceDetachOutcome::NotPersisted);
    }

    #[tokio::test]
    async fn attached_sm_demotion_is_effective_when_the_connection_is_unreachable() {
        let local_claims = SmSessionLocalClaims::new();
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        let connections = Arc::new(ConnectionRegistry::new());
        let stream_id = "wedged-demotion";
        let publication = registry
            .ensure_session_claim(stream_id)
            .await
            .expect("attached claim");
        drop(publication);
        local_claims.wire(Arc::clone(&registry));
        local_claims.wire_connection_registry(Arc::clone(&connections));

        let jid: FullJid = "alice@example.com/wedged-demotion".parse().expect("jid");
        let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel(1);
        let owner = connections.register(jid.clone(), outbound_tx);
        assert!(connections.set_sm_stream_id_if_owner(
            &jid,
            &owner,
            Some(waddle_xmpp::pending_delivery::SmSessionId::new(stream_id)),
        ));
        let entry = connections
            .entry_if_owner(&jid, &owner)
            .expect("attached entry");
        // Simulate a torn-down/wedged connection: its force-detach receiver
        // is gone, so the best-effort signal cannot be delivered.
        drop(
            entry
                .take_force_detach_rx()
                .expect("connection owns force-detach receiver"),
        );

        local_claims
            .demote(&Entity::new(EntityType::SmSession, stream_id))
            .await;

        assert_eq!(
            registry.current_sm_claim_fence(stream_id),
            None,
            "an unreachable connection must not leave the demoted fence behind"
        );
        assert!(local_claims.owned().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn attached_sm_demotion_delivers_the_signal_once_a_full_channel_drains() {
        let local_claims = SmSessionLocalClaims::new();
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        let connections = Arc::new(ConnectionRegistry::new());
        let stream_id = "full-channel-demotion";
        let publication = registry
            .ensure_session_claim(stream_id)
            .await
            .expect("attached claim");
        drop(publication);
        local_claims.wire(Arc::clone(&registry));
        local_claims.wire_connection_registry(Arc::clone(&connections));

        let jid: FullJid = "alice@example.com/full-channel-demotion"
            .parse()
            .expect("jid");
        let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel(1);
        let owner = connections.register(jid.clone(), outbound_tx);
        assert!(connections.set_sm_stream_id_if_owner(
            &jid,
            &owner,
            Some(waddle_xmpp::pending_delivery::SmSessionId::new(stream_id)),
        ));
        let entry = connections
            .entry_if_owner(&jid, &owner)
            .expect("attached entry");
        let mut force_detach_rx = entry
            .take_force_detach_rx()
            .expect("connection owns force-detach receiver");
        // Saturate the bounded channel so the demotion signal cannot be
        // accepted synchronously; a try_send here would drop it on the floor.
        let mut fillers = Vec::new();
        loop {
            let (ack, ack_rx) = tokio::sync::oneshot::channel();
            let filler = ForceDetachRequest {
                origin: ForceDetachOrigin::OwnerManagedRetirement,
                requester_bare_jid: jid.to_bare(),
                ack,
            };
            match entry.force_detach_sender().try_send(filler) {
                Ok(()) => fillers.push(ack_rx),
                Err(_) => break,
            }
        }

        local_claims
            .demote(&Entity::new(EntityType::SmSession, stream_id))
            .await;
        // Local demotion is complete even while the signal is still parked
        // behind the full channel.
        assert_eq!(registry.current_sm_claim_fence(stream_id), None);

        // Drain the fillers; the demotion request must then arrive intact.
        let mut saw_demotion_request = false;
        for _ in 0..=fillers.len() {
            let request = force_detach_rx.recv().await.expect("queued request");
            assert_eq!(request.origin, ForceDetachOrigin::OwnerManagedRetirement);
            saw_demotion_request = true;
            let _ = request.ack.send(ForceDetachOutcome::NotPersisted);
        }
        assert!(saw_demotion_request);
    }

    #[tokio::test]
    async fn detached_sm_demotion_keeps_immediate_local_forget_behavior() {
        let local_claims = SmSessionLocalClaims::new();
        let registry = Arc::new(InMemorySmSessionRegistry::new());
        local_claims.wire(Arc::clone(&registry));
        local_claims.wire_connection_registry(Arc::new(ConnectionRegistry::new()));
        let stream_id = "detached-demotion";
        registry
            .store_session(test_session(
                stream_id,
                "alice@example.com/detached-demotion",
            ))
            .await
            .expect("detached session");

        local_claims
            .demote(&Entity::new(EntityType::SmSession, stream_id))
            .await;

        assert!(local_claims.owned().await.is_empty());
        assert!(registry
            .peek_session(stream_id)
            .await
            .expect("detached lookup")
            .is_none());
    }

    #[tokio::test]
    async fn health_check_is_trivially_true() {
        let local_claims = SmSessionLocalClaims::new();
        let entity = Entity::new(EntityType::SmSession, "whatever".to_string());
        assert!(local_claims.health_check(&entity).await);
    }

    fn user_jid(localpart: &str) -> BareJid {
        format!("{localpart}@example.com")
            .parse()
            .expect("valid user bare JID")
    }

    fn spawn_user_registry() -> ActorRef<UserRegistryActor> {
        UserRegistryActor::spawn(UserRegistryActor::new())
    }

    #[tokio::test]
    async fn user_owned_reflects_the_wired_registry_users() {
        let user_local_claims = UserLocalClaims::new();
        let registry = spawn_user_registry();
        user_local_claims.wire(registry.clone());

        let jid = user_jid("owned-user");
        registry
            .ask(waddle_xmpp::registry::GetOrCreateUser {
                bare_jid: jid.clone(),
            })
            .await
            .expect("create user actor");

        let owned = user_local_claims.owned().await;
        assert_eq!(
            owned,
            vec![Entity::new(EntityType::UserActor, jid.to_string())]
        );
    }

    /// #1680 regression: a locally hosted resource whose bare-JID
    /// UserActor claim belongs to ANOTHER node (a same-bare remote-hosted
    /// resource) has a ConnectionRegistry socket but — by construction —
    /// no local user-registry mirror entry. `owned()` must NOT report it
    /// as a locally claimed entity when the registry listing succeeded:
    /// reconcile would demote it on every node-lease tick and
    /// force-detach the healthy live socket with `<conflict/>`.
    #[tokio::test]
    async fn user_owned_excludes_foreign_owned_connections_when_registry_lists() {
        let user_local_claims = UserLocalClaims::new();
        let registry = spawn_user_registry();
        let connection_registry = Arc::new(ConnectionRegistry::new());
        user_local_claims.wire(registry.clone());
        user_local_claims.wire_connection_registry(Arc::clone(&connection_registry));

        let mirrored = user_jid("locally-owned");
        registry
            .ask(waddle_xmpp::registry::GetOrCreateUser {
                bare_jid: mirrored.clone(),
            })
            .await
            .expect("create locally owned user actor");

        let remote_hosted: FullJid = "foreign-owned@example.com/deviceB"
            .parse()
            .expect("valid full JID");
        let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel(4);
        connection_registry.register(remote_hosted.clone(), outbound_tx);

        assert_eq!(
            user_local_claims.owned().await,
            vec![Entity::new(EntityType::UserActor, mirrored.to_string())],
            "a connection-registry-only bare JID is remote-hosted and must not \
             be handed to reconcile as a local claim"
        );
    }

    /// #1680: reconcile enumeration must be authoritative-only — an
    /// un-enumerable user registry reports NOTHING (the tick skips users)
    /// rather than falling back to the transport sweep, which would feed
    /// foreign-owned connections to reconcile.
    #[tokio::test]
    async fn user_owned_reports_nothing_when_the_registry_cannot_be_enumerated() {
        let user_local_claims = UserLocalClaims::new();
        let connection_registry = Arc::new(ConnectionRegistry::new());
        user_local_claims.wire_connection_registry(Arc::clone(&connection_registry));

        let jid: FullJid = "fallback-owned@example.com/phone"
            .parse()
            .expect("valid full JID");
        let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel(4);
        connection_registry.register(jid.clone(), outbound_tx);

        assert_eq!(
            user_local_claims.owned().await,
            Vec::new(),
            "reconcile input must never contain transport-sweep entities (#1680)"
        );
    }

    #[tokio::test]
    async fn user_terminal_sweep_includes_connection_registry_resources() {
        let user_local_claims = UserLocalClaims::new();
        let connection_registry = Arc::new(ConnectionRegistry::new());
        user_local_claims.wire_connection_registry(Arc::clone(&connection_registry));

        let jid: FullJid = "fallback-owned@example.com/phone"
            .parse()
            .expect("valid full JID");
        let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel(4);
        connection_registry.register(jid.clone(), outbound_tx);

        assert_eq!(
            user_local_claims.terminal_sweep().await,
            vec![Entity::new(EntityType::UserActor, jid.to_bare().to_string())],
            "terminal self-fence must still see live user resources when the user registry cannot be enumerated"
        );
    }

    #[tokio::test]
    async fn user_health_check_is_unhealthy_when_no_live_actor_exists() {
        let user_local_claims = UserLocalClaims::new();
        let registry = spawn_user_registry();
        user_local_claims.wire(registry);

        let entity = Entity::new(EntityType::UserActor, user_jid("ghost").to_string());
        assert!(
            !user_local_claims.health_check(&entity).await,
            "no live local UserActor must not veto a steal intent"
        );
    }

    #[tokio::test]
    async fn user_health_check_is_healthy_for_a_live_user_actor() {
        let user_local_claims = UserLocalClaims::new();
        let registry = spawn_user_registry();
        user_local_claims.wire(registry.clone());

        let jid = user_jid("healthy-user");
        registry
            .ask(waddle_xmpp::registry::GetOrCreateUser {
                bare_jid: jid.clone(),
            })
            .await
            .expect("create user actor");

        let entity = Entity::new(EntityType::UserActor, jid.to_string());
        assert!(
            user_local_claims.health_check(&entity).await,
            "a live UserActor must answer the health ask"
        );
    }

    #[tokio::test]
    async fn failed_user_health_then_demote_does_not_release_the_claim() {
        let user_local_claims = UserLocalClaims::new();
        let registry = spawn_user_registry();
        user_local_claims.wire(registry.clone());

        let claim_store: Arc<dyn waddle_xmpp::ownership::ClaimStore> =
            Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new());
        let node_identity = waddle_xmpp::ownership::SharedNodeIdentity::new(
            waddle_xmpp::ownership::NodeIdentity::local(),
        );
        registry
            .ask(waddle_xmpp::registry::WireUserClusteringClaims {
                claim_store: Arc::clone(&claim_store),
                node_identity,
            })
            .await
            .expect("wire claims");

        let jid = user_jid("health-failed");
        let actor_ref = registry
            .ask(waddle_xmpp::registry::GetOrCreateUser {
                bare_jid: jid.clone(),
            })
            .await
            .expect("create user actor");
        let entity = Entity::new(EntityType::UserActor, jid.to_string());
        actor_ref.kill();
        actor_ref.wait_for_shutdown().await;

        assert!(
            !user_local_claims.health_check(&entity).await,
            "dead actor must report unhealthy without releasing the durable claim"
        );
        user_local_claims.demote(&entity).await;

        assert!(
            claim_store
                .current_claim(&entity)
                .await
                .expect("current claim")
                .is_some(),
            "health-fail followed by demotion must not release a claim that may already have moved"
        );
    }

    #[tokio::test]
    async fn user_demote_hard_kills_and_forgets_the_local_actor() {
        let user_local_claims = UserLocalClaims::new();
        let registry = spawn_user_registry();
        user_local_claims.wire(registry.clone());

        let jid = user_jid("deposed-user");
        let actor_ref = registry
            .ask(waddle_xmpp::registry::GetOrCreateUser {
                bare_jid: jid.clone(),
            })
            .await
            .expect("create user actor");
        assert!(actor_ref.is_alive());

        let entity = Entity::new(EntityType::UserActor, jid.to_string());
        user_local_claims.demote(&entity).await;
        actor_ref.wait_for_shutdown().await;

        assert!(
            user_local_claims.owned().await.is_empty(),
            "demote must remove the local UserActor from ownership enumeration"
        );
        assert!(
            !user_local_claims.health_check(&entity).await,
            "a demoted UserActor must no longer report healthy"
        );
    }

    #[tokio::test]
    async fn user_demote_force_detaches_connection_registry_resources() {
        let user_local_claims = UserLocalClaims::new();
        let registry = spawn_user_registry();
        let connection_registry = Arc::new(ConnectionRegistry::new());
        user_local_claims.wire(registry.clone());
        user_local_claims.wire_connection_registry(Arc::clone(&connection_registry));

        let jid: FullJid = "force-close@example.com/phone"
            .parse()
            .expect("valid full JID");
        let bare_jid = jid.to_bare();
        let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel(4);
        let owner = connection_registry.register(jid.clone(), outbound_tx);
        let entry = connection_registry
            .entry_if_owner(&jid, &owner)
            .expect("registered entry");
        let mut force_detach_rx = entry
            .take_force_detach_rx()
            .expect("connection task owns force-detach receiver");
        let force_detach_task = tokio::spawn({
            let expected_bare = bare_jid.clone();
            async move {
                let request = force_detach_rx.recv().await.expect("force-detach request");
                assert_eq!(request.origin, ForceDetachOrigin::OwnerManagedRetirement);
                assert_eq!(request.requester_bare_jid, expected_bare);
                let _ = request.ack.send(ForceDetachOutcome::NotPersisted);
            }
        });

        registry
            .ask(waddle_xmpp::registry::RegisterUserResource {
                jid: jid.clone(),
                entry,
            })
            .await
            .expect("register user resource");
        assert!(connection_registry.is_connected(&jid));

        let entity = Entity::new(EntityType::UserActor, bare_jid.to_string());
        user_local_claims.demote(&entity).await;
        force_detach_task.await.expect("force-detach task");

        assert!(
            !connection_registry.is_connected(&jid),
            "demotion must remove the real ConnectionRegistry resource, not only the UserActor clone"
        );
        assert!(
            registry
                .ask(waddle_xmpp::registry::GetUser {
                    bare_jid: bare_jid.clone()
                })
                .await
                .expect("get user")
                .is_none(),
            "demotion must forget the local UserActor entry too"
        );
    }

    #[tokio::test]
    async fn combined_local_claims_routes_user_actor_operations() {
        let sm_local_claims = SmSessionLocalClaims::new();
        let room_local_claims = RoomLocalClaims::new();
        let user_local_claims = UserLocalClaims::new();
        let registry = spawn_user_registry();
        user_local_claims.wire(registry.clone());

        let jid = user_jid("combined-user");
        let actor_ref = registry
            .ask(waddle_xmpp::registry::GetOrCreateUser {
                bare_jid: jid.clone(),
            })
            .await
            .expect("create user actor");
        let entity = Entity::new(EntityType::UserActor, jid.to_string());
        let combined =
            CombinedLocalClaims::new(sm_local_claims, room_local_claims, user_local_claims);

        assert!(combined.owned().await.contains(&entity));
        assert!(combined.health_check(&entity).await);
        assert!(
            !combined.seal_before_release(&entity).await,
            "UserActor has no generic drain seal barrier yet"
        );

        combined.demote(&entity).await;
        actor_ref.wait_for_shutdown().await;
        assert!(!combined.health_check(&entity).await);
    }

    // -----------------------------------------------------------------
    // ADR-0017 Phase 3 Slice 7 FIX 3 (council-adjudicated):
    // `RoomLocalClaims::health_check` must report UNHEALTHY, never
    // healthy, when this node's claim has no live local actor to ask —
    // a wedged/poisoned/gone room must not veto another node's
    // legitimate steal-intent.
    // -----------------------------------------------------------------

    fn test_occupant_id_secret() -> waddle_xmpp::xep::xep0421::OccupantIdSecret {
        waddle_xmpp::xep::xep0421::OccupantIdSecret::new(vec![7u8; 32]).expect("valid secret")
    }

    #[tokio::test]
    async fn room_health_check_is_unhealthy_when_no_live_actor_exists() {
        let room_local_claims = RoomLocalClaims::new();
        let registry = waddle_xmpp::muc::RoomRegistry::spawn(
            "muc.example.com".to_string(),
            test_occupant_id_secret(),
            None,
        );
        room_local_claims.wire(registry);

        // No room was ever created for this JID at all — `owned()` would
        // never report it, but `health_check` must be defensively
        // unhealthy too (this models the residual window where Postgres
        // still attributes the claim to this node but the local registry
        // has no record of a live actor for it).
        let entity = Entity::new(EntityType::RoomActor, "ghost@muc.example.com".to_string());
        assert!(
            !room_local_claims.health_check(&entity).await,
            "no live local actor must report unhealthy, never healthy"
        );
    }

    #[tokio::test]
    async fn room_health_check_is_unhealthy_for_a_poisoned_room_actor() {
        // The Err(RoomActorStateLost) branch: a room whose actor died and
        // was poisoned in the registry. A wedged/poisoned room must never
        // report healthy — that would let the veto scan clear another
        // node's legitimate steal-intent against a claim this node can no
        // longer serve (Slice 7 FIX 3).
        let room_local_claims = RoomLocalClaims::new();
        let registry = waddle_xmpp::muc::RoomRegistry::spawn(
            "muc.example.com".to_string(),
            test_occupant_id_secret(),
            None,
        );
        room_local_claims.wire(registry.clone());

        let room_jid: jid::BareJid = "poisoned@muc.example.com".parse().expect("valid jid");
        let actor = registry
            .get_or_create_room(
                room_jid.clone(),
                "waddle-1".to_string(),
                "channel-1".to_string(),
                waddle_xmpp::muc::RoomConfig::default(),
            )
            .await
            .expect("create room")
            .actor_ref;
        actor.kill();
        actor.wait_for_shutdown().await;

        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        assert!(
            !room_local_claims.health_check(&entity).await,
            "a poisoned room actor must report unhealthy, never healthy"
        );
    }

    #[tokio::test]
    async fn room_health_check_is_healthy_for_a_live_room_actor() {
        let room_local_claims = RoomLocalClaims::new();
        let registry = waddle_xmpp::muc::RoomRegistry::spawn(
            "muc.example.com".to_string(),
            test_occupant_id_secret(),
            None,
        );
        room_local_claims.wire(registry.clone());

        let room_jid: jid::BareJid = "live@muc.example.com".parse().expect("valid jid");
        registry
            .get_or_create_room(
                room_jid.clone(),
                "waddle-1".to_string(),
                "channel-1".to_string(),
                waddle_xmpp::muc::RoomConfig::default(),
            )
            .await
            .expect("create room");

        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        assert!(
            room_local_claims.health_check(&entity).await,
            "a live, responsive RoomActor must report healthy"
        );
    }

    #[tokio::test]
    async fn room_demote_hard_kills_the_local_actor() {
        let room_local_claims = RoomLocalClaims::new();
        let registry = waddle_xmpp::muc::RoomRegistry::spawn(
            "muc.example.com".to_string(),
            test_occupant_id_secret(),
            None,
        );
        room_local_claims.wire(registry.clone());

        let room_jid: jid::BareJid = "deposed@muc.example.com".parse().expect("valid jid");
        let actor_ref = registry
            .get_or_create_room(
                room_jid.clone(),
                "waddle-1".to_string(),
                "channel-1".to_string(),
                waddle_xmpp::muc::RoomConfig::default(),
            )
            .await
            .expect("create room")
            .actor_ref;
        assert!(actor_ref.is_alive());

        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        room_local_claims.demote(&entity).await;

        // `ActorRef::kill()` aborts the underlying tokio task; the abort
        // takes effect at the task's next yield point rather than
        // synchronously, so poll briefly rather than asserting
        // immediately.
        let killed = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while actor_ref.is_alive() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok();
        assert!(killed, "demote must hard-kill the deposed RoomActor");
    }

    // -----------------------------------------------------------------
    // ADR-0017 Phase 3 Slice 10 FIX 1 (council-adjudicated): the real
    // `seal_before_release` barrier. Mirrors the `health_check` test
    // trio above (no-live-actor / poisoned / healthy), plus a dedicated
    // ordering test proving the barrier's actual purpose: a mutation
    // already queued ahead of the drain's `seal_before_release` ask has
    // its durable write committed before that ask returns.
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn room_seal_before_release_is_unsealed_when_no_live_actor_exists() {
        let room_local_claims = RoomLocalClaims::new();
        let registry = waddle_xmpp::muc::RoomRegistry::spawn(
            "muc.example.com".to_string(),
            test_occupant_id_secret(),
            None,
        );
        room_local_claims.wire(registry);

        // No room was ever created for this JID — mirrors
        // `room_health_check_is_unhealthy_when_no_live_actor_exists`: the
        // seal barrier must fail closed (leave the claim held) rather than
        // report a phantom room sealed.
        let entity = Entity::new(EntityType::RoomActor, "ghost@muc.example.com".to_string());
        assert!(
            !room_local_claims.seal_before_release(&entity).await,
            "no live local actor must report unsealed, never sealed"
        );
    }

    #[tokio::test]
    async fn room_seal_before_release_is_unsealed_for_a_dead_room_actor() {
        // FIX 1(b): a hung/dead room's seal must return false, so
        // `run_shutdown_drain` abandons (leaves claimed) rather than
        // releases it.
        let room_local_claims = RoomLocalClaims::new();
        let registry = waddle_xmpp::muc::RoomRegistry::spawn(
            "muc.example.com".to_string(),
            test_occupant_id_secret(),
            None,
        );
        room_local_claims.wire(registry.clone());

        let room_jid: jid::BareJid = "dead@muc.example.com".parse().expect("valid jid");
        let actor_ref = registry
            .get_or_create_room(
                room_jid.clone(),
                "waddle-1".to_string(),
                "channel-1".to_string(),
                waddle_xmpp::muc::RoomConfig::default(),
            )
            .await
            .expect("create room")
            .actor_ref;
        actor_ref.kill();
        actor_ref.wait_for_shutdown().await;

        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        assert!(
            !room_local_claims.seal_before_release(&entity).await,
            "a dead room actor must report unsealed, never sealed"
        );
    }

    #[tokio::test]
    async fn room_seal_before_release_is_sealed_for_a_healthy_room_actor() {
        let room_local_claims = RoomLocalClaims::new();
        let registry = waddle_xmpp::muc::RoomRegistry::spawn(
            "muc.example.com".to_string(),
            test_occupant_id_secret(),
            None,
        );
        room_local_claims.wire(registry.clone());

        let room_jid: jid::BareJid = "healthy@muc.example.com".parse().expect("valid jid");
        registry
            .get_or_create_room(
                room_jid.clone(),
                "waddle-1".to_string(),
                "channel-1".to_string(),
                waddle_xmpp::muc::RoomConfig::default(),
            )
            .await
            .expect("create room");

        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        assert!(
            room_local_claims.seal_before_release(&entity).await,
            "a live, responsive RoomActor must report sealed"
        );
    }

    /// A `MucDurableStore` test double whose `save_config` signals
    /// `started` the instant it is entered (so a test can observe that
    /// the persist call is genuinely in flight) and only records
    /// `persisted = true` after an artificial delay — modeling a
    /// durable write that is slow, not instantaneous.
    struct RecordingDurableStore {
        started: Arc<tokio::sync::Notify>,
        persisted: Arc<std::sync::atomic::AtomicBool>,
    }

    fn expected_local_room_fence(
        room_jid: &jid::BareJid,
    ) -> waddle_xmpp::muc::RoomClaimFenceContext {
        waddle_xmpp::muc::RoomClaimFenceContext::new(
            waddle_xmpp::ownership::Entity::new(
                waddle_xmpp::ownership::EntityType::RoomActor,
                room_jid.to_string(),
            ),
            waddle_xmpp::ownership::NodeIdentity::local(),
            waddle_xmpp::ownership::ClaimEpoch(0),
        )
    }

    fn validate_local_room_fence(
        room_jid: &jid::BareJid,
        fence: &waddle_xmpp::muc::RoomClaimFenceContext,
    ) -> Result<(), waddle_xmpp::XmppError> {
        if fence == &expected_local_room_fence(room_jid) {
            Ok(())
        } else {
            Err(waddle_xmpp::XmppError::internal(
                "test store received an unexpected room claim fence",
            ))
        }
    }

    impl waddle_xmpp::muc::MucDurableStore for RecordingDurableStore {
        fn load_room_state_fenced<'a>(
            &'a self,
            room_jid: &'a jid::BareJid,
            fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        ) -> waddle_xmpp::muc::MucDurableFuture<'a, Option<waddle_xmpp::muc::DurableRoomState>>
        {
            let validation = validate_local_room_fence(room_jid, fence);
            Box::pin(async move {
                validation?;
                Ok(None)
            })
        }

        fn commit_room_mutation<'a>(
            &'a self,
            room_jid: &'a jid::BareJid,
            fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
            intent: waddle_xmpp::muc::RoomDurableMutation,
            _effects: waddle_xmpp::muc::RoomMutationEffects,
        ) -> waddle_xmpp::muc::RoomCommitFuture<'a> {
            if let Err(error) = validate_local_room_fence(room_jid, fence) {
                return Box::pin(async move {
                    let _ = error;
                    Err(waddle_xmpp::muc::RoomCommitError::NotOwner)
                });
            }
            // Preparation-time Create/Activate commits ride this store too
            // (#1645); only the Config commit under test participates in
            // the started/persisted ordering proof.
            let is_config = matches!(intent, waddle_xmpp::muc::RoomDurableMutation::Config { .. });
            Box::pin(async move {
                if is_config {
                    self.started.notify_one();
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    self.persisted
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                }
                Ok(waddle_xmpp::muc::RoomCommitOutcome {
                    coordinates: waddle_xmpp::muc::RoomCommittedCoordinates {
                        lifecycle: waddle_xmpp::muc::RoomLifecycleId::generate(),
                        revision: waddle_xmpp::muc::RoomRevision::initial(),
                    },
                    reservation: None,
                })
            })
        }

        fn check_exact_claim_fence<'a>(
            &'a self,
            room_jid: &'a jid::BareJid,
            fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        ) -> waddle_xmpp::muc::MucDurableFuture<'a, bool> {
            let matches = fence == &expected_local_room_fence(room_jid);
            Box::pin(async move { Ok(matches) })
        }
    }

    /// ADR-0017 Phase 3 Slice 10 FIX 1(a) (council-adjudicated): the
    /// exact invariant the seal barrier exists to prove — a mutation
    /// (`UpdateConfig`) already enqueued in the room's mailbox ahead of
    /// the drain's `seal_before_release` ask has its durable write
    /// committed BEFORE that ask returns, never after. Proven
    /// deterministically (no sleep-based race): `RecordingDurableStore`
    /// signals `started` the instant its `save_config` is entered —
    /// which, because kameo processes a mailbox strictly in order, can
    /// only happen once the `UpdateConfig` message has already been
    /// dequeued and its handler is running. The test waits for that
    /// signal (proving `UpdateConfig`'s handler is now suspended inside
    /// the still-in-flight persist call) before issuing the
    /// `seal_before_release` ask — so `HealthCheck` cannot itself be
    /// dequeued and answered until `UpdateConfig`'s handler, persist
    /// `.await` and all, has returned.
    #[tokio::test]
    async fn seal_before_release_proves_a_queued_ahead_mutation_already_committed() {
        let room_local_claims = RoomLocalClaims::new();
        let registry = waddle_xmpp::muc::RoomRegistry::spawn(
            "muc.example.com".to_string(),
            test_occupant_id_secret(),
            None,
        );
        room_local_claims.wire(registry.clone());

        let started = Arc::new(tokio::sync::Notify::new());
        let persisted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let durable_store: std::sync::Arc<dyn waddle_xmpp::muc::MucDurableStore> =
            std::sync::Arc::new(RecordingDurableStore {
                started: Arc::clone(&started),
                persisted: Arc::clone(&persisted),
            });
        registry
            .wire_clustering_claims(
                std::sync::Arc::new(waddle_xmpp::ownership::InProcessClaimStore::new()),
                waddle_xmpp::ownership::SharedNodeIdentity::new(
                    waddle_xmpp::ownership::NodeIdentity::local(),
                ),
                Some(durable_store),
                None,
            )
            .await;

        let room_jid: jid::BareJid = "queued@muc.example.com".parse().expect("valid jid");
        let actor_ref = registry
            .get_or_create_room(
                room_jid.clone(),
                "waddle-1".to_string(),
                "channel-1".to_string(),
                waddle_xmpp::muc::RoomConfig::default(),
            )
            .await
            .expect("create room")
            .actor_ref;

        // Fire `UpdateConfig` in the background — never awaited before
        // `seal_before_release` is issued below, so the two race exactly
        // as the drain scenario this fix targets does (a mutation
        // enqueued ahead of the drain snapshot).
        let update_task = tokio::spawn({
            let actor_ref = actor_ref.clone();
            async move {
                actor_ref
                    .ask(waddle_xmpp::muc::room_actor::UpdateConfig {
                        config: waddle_xmpp::muc::RoomConfig {
                            persistent: true,
                            ..waddle_xmpp::muc::RoomConfig::default()
                        },
                        effect_plan: waddle_xmpp::muc::room_actor::ConfigEffectPlan::DirectAudience,
                    })
                    .await
            }
        });

        // Block until `UpdateConfig`'s handler is provably mid-persist —
        // i.e. already dequeued, gate_mutation passed, in-memory config
        // already mutated, and now suspended inside the durable write.
        started.notified().await;
        assert!(
            !persisted.load(std::sync::atomic::Ordering::SeqCst),
            "sanity: the persist call must still be in flight at this point"
        );

        let entity = Entity::new(EntityType::RoomActor, room_jid.to_string());
        let sealed = room_local_claims.seal_before_release(&entity).await;

        assert!(sealed, "a healthy room's seal must succeed");
        assert!(
            persisted.load(std::sync::atomic::Ordering::SeqCst),
            "seal_before_release must not return until the mutation queued ahead of it \
             (UpdateConfig) has already committed its durable write — losing this ordering \
             is exactly the inversion FIX 1 closes"
        );

        update_task
            .await
            .expect("update task")
            .expect("UpdateConfig must have succeeded");
    }
}
