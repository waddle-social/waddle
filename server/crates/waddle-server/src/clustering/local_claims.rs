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
//!
//! **Scope note**: this module reports SM-session, RoomActor, and UserActor
//! claims to the generic node-lease/self-fence machinery. UserActor local
//! claims use the same fill-in-later cell pattern as SM sessions: the
//! registry is created later in `server/http.rs`, then wired into the handle
//! that `start_if_enabled` already handed to `run_node_lease`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::join_all;
use jid::{BareJid, FullJid};
use kameo::actor::ActorRef;
use waddle_xmpp::muc::room_actor::HealthCheck;
use waddle_xmpp::muc::RoomRegistry;
use waddle_xmpp::ownership::{Entity, EntityType};
use waddle_xmpp::registry::user_actor::HealthCheck as UserHealthCheck;
use waddle_xmpp::registry::{
    ConnectionEntry, ConnectionPlacement, ConnectionRegistry, DemoteAllUserActors, DemoteUserActor,
    ForceDetachOutcome, ForceDetachReason, ForceDetachRequest, GetUserForLocalClaim, ListUsers,
    UserRegistryActor,
};
use waddle_xmpp::stream_management::{InMemorySmSessionRegistry, ReclaimedSessionHydration};

use super::self_fence::{LocallyClaimedEntities, ReclaimedEntityHydration};

/// Bound on the health-ask this impl issues against a locally-claimed
/// room's `RoomActor` (ADR-0017 Phase 3 Slice 7's `RoomActor` counterpart
/// of the `UserActor` owner-veto path). Chosen well below the steal-intent
/// TTL / heartbeat interval so a genuinely wedged room is detected and
/// hard-killed within one veto-scan tick, never straddling two.
const ROOM_HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const USER_HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const USER_FORCE_DETACH_ACK_TIMEOUT: Duration = Duration::from_secs(2);
const USER_HARD_RETIRE_TIMEOUT: Duration = Duration::from_secs(1);

/// See the module doc for the construction-order rationale.
pub struct SmSessionLocalClaims {
    registry: OnceLock<Arc<InMemorySmSessionRegistry>>,
    initialized: AtomicBool,
}

impl SmSessionLocalClaims {
    /// Construct empty. See the module doc's construction-order note.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            registry: OnceLock::new(),
            initialized: AtomicBool::new(false),
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
            return;
        }
        // `wire` is intentionally called only after startup restoration has
        // completed. Release-publishing this bit makes a concurrent
        // self-fence retry observe both the registry pointer and all sessions
        // restored before it is allowed to rotate the node epoch.
        self.initialized.store(true, Ordering::Release);
    }
}

#[async_trait]
impl LocallyClaimedEntities for SmSessionLocalClaims {
    async fn owned(&self) -> Vec<Entity> {
        let Some(registry) = self.registry.get() else {
            return Vec::new();
        };
        registry
            .live_session_ids()
            .unwrap_or_default()
            .into_iter()
            .map(|stream_id| Entity::new(EntityType::SmSession, stream_id))
            .collect()
    }

    async fn demote_all_on_self_fence(&self) -> bool {
        if !self.initialized.load(Ordering::Acquire) {
            tracing::warn!("SM local claims are not initialized; refusing node-epoch recovery");
            return false;
        }
        let Some(registry) = self.registry.get() else {
            return false;
        };
        let Some(stream_ids) = registry.live_session_ids() else {
            return false;
        };
        for stream_id in stream_ids {
            if !registry.forget_claim_locally(&stream_id).await {
                return false;
            }
        }
        true
    }

    async fn demote(&self, entity: &Entity) {
        let Some(registry) = self.registry.get() else {
            return;
        };
        if entity.entity_type != EntityType::SmSession {
            return;
        }
        registry.forget_claim_locally(&entity.id).await;
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

    /// FIX 4(b) (ADR-0017 Phase 3 Slice 5 corrigenda): delegates straight
    /// to `InMemorySmSessionRegistry::hydrate_reclaimed` — the same
    /// targeted, per-entity-shard-locked hydration path the orphan reaper
    /// uses (FIX 2), so `self_fence::run_node_lease`'s inline post-fence
    /// reclaim and the general reaper share one hydration implementation.
    /// A no-op before `wire` runs, mirroring `demote`/`owned`'s identical
    /// unwired behavior.
    async fn hydrate_reclaimed(
        &self,
        owner: &waddle_xmpp::ownership::NodeIdentity,
        entity: &Entity,
        epoch: waddle_xmpp::ownership::ClaimEpoch,
    ) -> ReclaimedEntityHydration {
        if !self.initialized.load(Ordering::Acquire) {
            return ReclaimedEntityHydration::Retry;
        }
        let Some(registry) = self.registry.get() else {
            return ReclaimedEntityHydration::Retry;
        };
        match registry
            .hydrate_reclaimed_one_as(entity, epoch, owner)
            .await
        {
            Ok(ReclaimedSessionHydration::Hydrated | ReclaimedSessionHydration::AlreadyLocal) => {
                ReclaimedEntityHydration::Local
            }
            Ok(ReclaimedSessionHydration::Elsewhere) => ReclaimedEntityHydration::Elsewhere,
            Ok(ReclaimedSessionHydration::TerminallyReleased) => {
                ReclaimedEntityHydration::TerminallyReleased
            }
            Ok(ReclaimedSessionHydration::Retry) => ReclaimedEntityHydration::Retry,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "SmSessionLocalClaims::hydrate_reclaimed: strict hydration failed"
                );
                ReclaimedEntityHydration::Retry
            }
        }
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
            Err(error) => {
                tracing::warn!(
                    id = %entity.id,
                    %error,
                    "RoomLocalClaims: entity id is not a valid room JID"
                );
                None
            }
        }
    }

    pub async fn demote_if_superseded(
        &self,
        entity: &Entity,
        expected_owner: waddle_xmpp::ownership::NodeIdentity,
        new_epoch: waddle_xmpp::ownership::ClaimEpoch,
    ) -> bool {
        let Some(registry) = self.registry.get() else {
            return false;
        };
        let Some(room_jid) = Self::room_jid(entity) else {
            return false;
        };
        match registry
            .demote_room_if_superseded(room_jid, expected_owner, new_epoch)
            .await
        {
            Ok(demoted) => demoted,
            Err(error) => {
                tracing::warn!(%error, %entity, "failed exact-incarnation RoomActor demotion");
                false
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
            Ok(jids) => jids
                .into_iter()
                .map(|jid| Entity::new(EntityType::RoomActor, jid.to_string()))
                .collect(),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "RoomLocalClaims::owned: room registry list_rooms failed; \
                     reporting no owned rooms this interval (fail-safe: a \
                     wedged/unreachable registry cannot report ownership it \
                     cannot itself observe)"
                );
                Vec::new()
            }
        }
    }

    async fn demote_all_on_self_fence(&self) -> bool {
        let Some(registry) = self.registry.get() else {
            return true;
        };
        match registry.demote_all_rooms().await {
            Ok(count) => {
                tracing::warn!(count, "demoted every local RoomActor after node self-fence");
                true
            }
            Err(error) => {
                tracing::error!(
                    %error,
                    "failed to demote every local RoomActor after node self-fence"
                );
                false
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
    /// the affiliation-bulk-apply path) synchronously `.await`s its own
    /// `gate_mutation()` check and durable persist call
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
    remote_resource_bridge: OnceLock<Arc<super::route_bridge::OrderedRelayDeliveryBridge>>,
}

impl UserLocalClaims {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            registry: OnceLock::new(),
            connection_registry: OnceLock::new(),
            remote_resource_bridge: OnceLock::new(),
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

    pub fn wire_remote_resource_bridge(
        &self,
        bridge: Arc<super::route_bridge::OrderedRelayDeliveryBridge>,
    ) {
        if self.remote_resource_bridge.set(bridge).is_err() {
            tracing::error!(
                "UserLocalClaims::wire_remote_resource_bridge called more than once; ignoring"
            );
        }
    }

    fn user_jid(entity: &Entity) -> Option<BareJid> {
        if entity.entity_type != EntityType::UserActor {
            return None;
        }
        match entity.id.parse::<BareJid>() {
            Ok(jid) => Some(jid),
            Err(error) => {
                tracing::warn!(
                    id = %entity.id,
                    %error,
                    "UserLocalClaims: entity id is not a valid user bare JID"
                );
                None
            }
        }
    }

    fn connection_registry_resources(
        connection_registry: &ConnectionRegistry,
    ) -> Vec<(FullJid, ConnectionEntry)> {
        connection_registry
            .list_connections()
            .into_iter()
            .filter_map(|jid| {
                let entry = connection_registry.get_entry(&jid)?;
                Some((jid, entry))
            })
            .collect()
    }

    fn merge_resource_targets(
        resources: &mut Vec<(FullJid, ConnectionEntry)>,
        extra: Vec<(FullJid, ConnectionEntry)>,
    ) {
        let mut seen: HashMap<FullJid, Vec<Arc<std::sync::atomic::AtomicBool>>> = HashMap::new();
        for (jid, entry) in resources.iter() {
            seen.entry(jid.clone())
                .or_default()
                .push(entry.carbons_handle());
        }
        for (jid, entry) in extra {
            let owner = entry.carbons_handle();
            let owners = seen.entry(jid.clone()).or_default();
            if owners.iter().any(|existing| Arc::ptr_eq(existing, &owner)) {
                continue;
            }
            owners.push(owner);
            resources.push((jid, entry));
        }
    }

    async fn force_detach_resources(
        &self,
        resources: Vec<(FullJid, ConnectionEntry)>,
        reason: ForceDetachReason,
    ) -> bool {
        if resources.is_empty() {
            return true;
        }
        let Some(connection_registry) = self.connection_registry.get() else {
            tracing::warn!(
                resource_count = resources.len(),
                ?reason,
                "UserLocalClaims::force_detach_resources: no ConnectionRegistry wired; cannot force-detach live resources"
            );
            return false;
        };

        let retirements = resources.into_iter().map(|(jid, entry)| async move {
            self.force_detach_resource(connection_registry, jid, entry, reason)
                .await
        });
        join_all(retirements)
            .await
            .into_iter()
            .all(|retired| retired)
    }

    async fn force_detach_resource(
        &self,
        connection_registry: &ConnectionRegistry,
        jid: FullJid,
        entry: ConnectionEntry,
        reason: ForceDetachReason,
    ) -> bool {
        let owner = entry.carbons_handle();
        let requester_bare_jid = jid.to_bare();
        let (ack, ack_rx) = tokio::sync::oneshot::channel();
        let request = ForceDetachRequest {
            requester_bare_jid: requester_bare_jid.clone(),
            reason,
            ack,
        };

        if entry.placement() == ConnectionPlacement::RemoteMirror {
            let relayed = match entry.force_detach_sender().try_send(request) {
                Ok(()) => matches!(
                    tokio::time::timeout(USER_FORCE_DETACH_ACK_TIMEOUT, ack_rx).await,
                    Ok(Ok(
                        ForceDetachOutcome::Detached | ForceDetachOutcome::NotPersisted
                    ))
                ),
                Err(error) => {
                    tracing::warn!(
                        jid = %jid,
                        ?reason,
                        ?error,
                        "remote-mirror force-detach control queue unavailable; attempting direct terminal compensation"
                    );
                    false
                }
            };
            let retired = if relayed {
                true
            } else {
                let Some(bridge) = self.remote_resource_bridge.get() else {
                    tracing::error!(
                        jid = %jid,
                        ?reason,
                        "remote-mirror force-detach is uncertain and no direct compensation bridge is wired"
                    );
                    return false;
                };
                bridge
                    .terminally_force_detach_remote_mirror_if_owner(&jid, &owner, reason)
                    .await
            };
            if !retired {
                tracing::error!(
                    jid = %jid,
                    ?reason,
                    "remote physical socket retirement remains uncertain; keeping terminal teardown incomplete"
                );
                return false;
            }
            connection_registry.unregister_if_owner(&jid, &owner);
            self.forget_remote_resource_state(&jid, &owner, entry.placement())
                .await;
            return true;
        }

        let retired = match entry.force_detach_sender().try_send(request) {
            Ok(()) => match tokio::time::timeout(USER_FORCE_DETACH_ACK_TIMEOUT, ack_rx).await {
                Ok(Ok(ForceDetachOutcome::Detached | ForceDetachOutcome::NotPersisted)) => {
                    connection_registry.unregister_if_owner(&jid, &owner);
                    tracing::debug!(
                        jid = %jid,
                        ?reason,
                        "local connection acknowledged force-detach"
                    );
                    true
                }
                Ok(Ok(ForceDetachOutcome::IdentityMismatch)) => {
                    tracing::error!(
                        jid = %jid,
                        requester = %requester_bare_jid,
                        ?reason,
                        "local connection refused force-detach identity; not hard-aborting"
                    );
                    false
                }
                Ok(Err(_closed)) => {
                    tracing::warn!(jid = %jid, ?reason, "force-detach ack channel closed; hard-retiring local socket");
                    Self::hard_retire_local_resource(connection_registry, &jid, &entry, &owner)
                        .await
                }
                Err(_elapsed) => {
                    tracing::warn!(
                        jid = %jid,
                        ?reason,
                        timeout_ms = USER_FORCE_DETACH_ACK_TIMEOUT.as_millis() as u64,
                        "force-detach timed out; hard-retiring local socket"
                    );
                    Self::hard_retire_local_resource(connection_registry, &jid, &entry, &owner)
                        .await
                }
            },
            Err(error) => {
                tracing::warn!(
                    jid = %jid,
                    ?reason,
                    ?error,
                    "force-detach request could not be queued; hard-retiring local socket"
                );
                Self::hard_retire_local_resource(connection_registry, &jid, &entry, &owner).await
            }
        };
        if retired {
            self.forget_remote_resource_state(&jid, &owner, entry.placement())
                .await;
        }
        retired
    }

    async fn forget_remote_resource_state(
        &self,
        jid: &FullJid,
        owner: &Arc<std::sync::atomic::AtomicBool>,
        placement: ConnectionPlacement,
    ) {
        if let Some(bridge) = self.remote_resource_bridge.get() {
            bridge
                .forget_remote_resource_state_if_owner(jid, owner, placement)
                .await;
        }
    }

    async fn hard_retire_local_resource(
        connection_registry: &ConnectionRegistry,
        jid: &FullJid,
        entry: &ConnectionEntry,
        owner: &Arc<std::sync::atomic::AtomicBool>,
    ) -> bool {
        let Some(retirement) = entry.retirement_handle() else {
            tracing::error!(
                jid = %jid,
                "local socket has no hard-retirement handle; keeping terminal teardown incomplete"
            );
            return false;
        };
        retirement.abort();
        if tokio::time::timeout(USER_HARD_RETIRE_TIMEOUT, retirement.terminated())
            .await
            .is_err()
        {
            tracing::error!(
                jid = %jid,
                timeout_ms = USER_HARD_RETIRE_TIMEOUT.as_millis() as u64,
                "hard-retired socket did not terminate; keeping terminal teardown incomplete"
            );
            return false;
        }
        connection_registry.unregister_if_owner(jid, owner);
        tracing::warn!(jid = %jid, "hard-retired non-cooperative local socket");
        true
    }

    async fn demote_user_actor(
        &self,
        bare_jid: &BareJid,
    ) -> Option<Vec<(FullJid, ConnectionEntry)>> {
        let Some(registry) = self.registry.get() else {
            return Some(Vec::new());
        };
        match registry
            .ask(DemoteUserActor {
                bare_jid: bare_jid.clone(),
            })
            .mailbox_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .reply_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .await
        {
            Ok(resources) => {
                tracing::warn!(
                    jid = %bare_jid,
                    resource_count = resources.len(),
                    "demoted (hard-killed) a locally-claimed UserActor: Postgres \
                     no longer attributes this user claim to this node"
                );
                Some(resources)
            }
            Err(error) => {
                tracing::warn!(
                    jid = %bare_jid,
                    ?error,
                    "UserLocalClaims::demote: user registry demotion failed"
                );
                None
            }
        }
    }

    async fn demote_all_user_actors(&self) -> Option<Vec<(FullJid, ConnectionEntry)>> {
        let Some(registry) = self.registry.get() else {
            return Some(Vec::new());
        };
        match registry
            .ask(DemoteAllUserActors)
            .mailbox_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .reply_timeout(USER_HEALTH_CHECK_TIMEOUT)
            .await
        {
            Ok(resources) => {
                tracing::warn!(
                    resource_count = resources.len(),
                    "demoted every local UserActor after node self-fence"
                );
                Some(resources)
            }
            Err(error) => {
                tracing::error!(
                    ?error,
                    "failed to demote every local UserActor after node self-fence"
                );
                None
            }
        }
    }
}

#[async_trait]
impl LocallyClaimedEntities for UserLocalClaims {
    async fn owned(&self) -> Vec<Entity> {
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
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "UserLocalClaims::owned: authoritative user registry enumeration failed; \
                     skipping UserActor reconciliation this interval"
                );
                Vec::new()
            }
        }
    }

    async fn demote_all_on_self_fence(&self) -> bool {
        let mut resources = self
            .connection_registry
            .get()
            .map(|registry| Self::connection_registry_resources(registry))
            .unwrap_or_default();
        let Some(actor_resources) = self.demote_all_user_actors().await else {
            return false;
        };
        Self::merge_resource_targets(&mut resources, actor_resources);
        if !self
            .force_detach_resources(resources, ForceDetachReason::NodeSelfFenced)
            .await
        {
            return false;
        }
        let Some(bridge) = self.remote_resource_bridge.get() else {
            return true;
        };
        tokio::time::timeout(
            USER_HARD_RETIRE_TIMEOUT,
            bridge.clear_remote_resource_state_on_self_fence(),
        )
        .await
        .is_ok()
    }

    async fn demote(&self, entity: &Entity) {
        let Some(bare_jid) = Self::user_jid(entity) else {
            return;
        };
        let Some(resources) = self.demote_user_actor(&bare_jid).await else {
            return;
        };
        let _ = self
            .force_detach_resources(resources, ForceDetachReason::OwnershipLost)
            .await;
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
            Err(error) => {
                tracing::warn!(
                    jid = %bare_jid,
                    ?error,
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

    async fn demote_all_on_self_fence(&self) -> bool {
        let (sm, room, user) = tokio::join!(
            self.sm.demote_all_on_self_fence(),
            self.room.demote_all_on_self_fence(),
            self.user.demote_all_on_self_fence(),
        );
        sm && room && user
    }

    async fn demote(&self, entity: &Entity) {
        match entity.entity_type {
            EntityType::SmSession => self.sm.demote(entity).await,
            EntityType::RoomActor => self.room.demote(entity).await,
            EntityType::UserActor => self.user.demote(entity).await,
        }
    }

    async fn health_check(&self, entity: &Entity) -> bool {
        match entity.entity_type {
            EntityType::SmSession => self.sm.health_check(entity).await,
            EntityType::RoomActor => self.room.health_check(entity).await,
            EntityType::UserActor => self.user.health_check(entity).await,
        }
    }

    async fn hydrate_reclaimed(
        &self,
        owner: &waddle_xmpp::ownership::NodeIdentity,
        entity: &Entity,
        epoch: waddle_xmpp::ownership::ClaimEpoch,
    ) -> ReclaimedEntityHydration {
        if entity.entity_type == EntityType::SmSession {
            self.sm.hydrate_reclaimed(owner, entity, epoch).await
        } else {
            ReclaimedEntityHydration::Retry
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
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry as _};

    fn test_session(stream_id: &str, jid: &str) -> DetachedSession {
        DetachedSession {
            stream_id: stream_id.to_string(),
            user_id: jid.to_string(),
            jid: jid.parse().expect("valid jid"),
            inbound_count: 0,
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
    async fn unwired_instance_blocks_terminal_recovery_until_post_restore_wire() {
        let local_claims = SmSessionLocalClaims::new();
        assert!(
            !local_claims.demote_all_on_self_fence().await,
            "an unwired startup registry must keep node-epoch recovery fenced"
        );

        local_claims.wire(Arc::new(InMemorySmSessionRegistry::new()));
        assert!(
            local_claims.demote_all_on_self_fence().await,
            "post-restore wiring publishes initialization to the recovery loop"
        );
    }

    #[tokio::test]
    async fn owned_reflects_the_wired_registry_live_session_ids() {
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

    #[tokio::test]
    async fn user_reconciliation_ownership_excludes_connection_only_resources() {
        let user_local_claims = UserLocalClaims::new();
        let connection_registry = Arc::new(ConnectionRegistry::new());
        user_local_claims.wire_connection_registry(Arc::clone(&connection_registry));

        let jid: FullJid = "fallback-owned@example.com/phone"
            .parse()
            .expect("valid full JID");
        let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel(4);
        connection_registry.register(jid.clone(), outbound_tx);

        assert!(
            user_local_claims.owned().await.is_empty(),
            "a physically hosted resource is not an authoritative local UserActor claim"
        );
    }

    #[tokio::test]
    async fn user_self_fence_force_detaches_connection_only_resources() {
        let user_local_claims = UserLocalClaims::new();
        let connection_registry = Arc::new(ConnectionRegistry::new());
        user_local_claims.wire_connection_registry(Arc::clone(&connection_registry));

        let jid: FullJid = "remote-socket@example.com/web"
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
        let force_detach_task = tokio::spawn(async move {
            let request = force_detach_rx.recv().await.expect("force-detach request");
            assert_eq!(request.requester_bare_jid, bare_jid);
            assert_eq!(request.reason, ForceDetachReason::NodeSelfFenced);
            let _ = request.ack.send(ForceDetachOutcome::NotPersisted);
        });

        assert!(user_local_claims.demote_all_on_self_fence().await);
        force_detach_task.await.expect("force-detach task");

        assert!(
            !connection_registry.is_connected(&jid),
            "whole-node self-fence must still close every socket physically hosted here"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn user_self_fence_force_detach_timeout_is_concurrent_across_resources() {
        let user_local_claims = UserLocalClaims::new();
        let connection_registry = Arc::new(ConnectionRegistry::new());
        user_local_claims.wire_connection_registry(Arc::clone(&connection_registry));

        let mut receivers = Vec::new();
        for resource in ["phone", "tablet", "desktop"] {
            let jid: FullJid = format!("remote-socket@example.com/{resource}")
                .parse()
                .expect("valid full JID");
            let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel(4);
            let owner = connection_registry.register(jid.clone(), outbound_tx);
            let entry = connection_registry
                .entry_if_owner(&jid, &owner)
                .expect("registered entry");
            receivers.push(
                entry
                    .take_force_detach_rx()
                    .expect("connection task owns force-detach receiver"),
            );
        }

        let teardown = tokio::spawn({
            let user_local_claims = Arc::clone(&user_local_claims);
            async move { user_local_claims.demote_all_on_self_fence().await }
        });
        let mut pending_requests = Vec::new();
        for receiver in &mut receivers {
            let request = receiver.recv().await.expect("force-detach request");
            assert_eq!(request.reason, ForceDetachReason::NodeSelfFenced);
            pending_requests.push(request);
        }

        tokio::time::advance(USER_FORCE_DETACH_ACK_TIMEOUT + Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(
            teardown.is_finished(),
            "three unresponsive resources must share one timeout window, not wait serially"
        );
        assert!(
            !teardown.await.expect("terminal teardown task"),
            "an unacknowledged physical socket must keep terminal teardown incomplete"
        );
        drop(pending_requests);
    }

    #[tokio::test(start_paused = true)]
    async fn user_self_fence_hard_retires_non_cooperative_local_socket() {
        let user_local_claims = UserLocalClaims::new();
        let connection_registry = Arc::new(ConnectionRegistry::new());
        user_local_claims.wire_connection_registry(Arc::clone(&connection_registry));

        let jid: FullJid = "wedged@example.com/phone".parse().expect("valid full JID");
        let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel(4);
        let owner = connection_registry.register(jid.clone(), outbound_tx);
        let entry = connection_registry
            .entry_if_owner(&jid, &owner)
            .expect("registered entry");
        let mut force_detach_rx = entry
            .take_force_detach_rx()
            .expect("connection owns force-detach receiver");
        let (abort, abort_registration) = futures::future::AbortHandle::new_pair();
        let terminated = tokio_util::sync::CancellationToken::new();
        assert!(entry.install_retirement_handle(
            waddle_xmpp::registry::ConnectionRetirementHandle::new(abort, terminated.clone(),)
        ));
        let connection_task = tokio::spawn(async move {
            let _ =
                futures::future::Abortable::new(std::future::pending::<()>(), abort_registration)
                    .await;
            terminated.cancel();
        });

        let teardown = tokio::spawn({
            let user_local_claims = Arc::clone(&user_local_claims);
            async move { user_local_claims.demote_all_on_self_fence().await }
        });
        let pending_request = force_detach_rx.recv().await.expect("force-detach request");
        tokio::time::advance(USER_FORCE_DETACH_ACK_TIMEOUT + Duration::from_millis(1)).await;
        tokio::task::yield_now().await;

        assert!(teardown.await.expect("terminal teardown task"));
        assert!(!connection_registry.is_connected(&jid));
        connection_task.await.expect("hard-retired connection task");
        drop(pending_request);
    }

    #[tokio::test]
    async fn user_self_fence_retires_remote_mirror_after_confirmed_socket_retirement() {
        let user_local_claims = UserLocalClaims::new();
        let connection_registry = Arc::new(ConnectionRegistry::new());
        user_local_claims.wire_connection_registry(Arc::clone(&connection_registry));

        let jid: FullJid = "remote-mirror@example.com/tablet"
            .parse()
            .expect("valid full JID");
        let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel(4);
        let entry = ConnectionEntry::new_remote_mirror(outbound_tx);
        let mut force_detach_rx = entry
            .take_force_detach_rx()
            .expect("mirror forwarder owns force-detach receiver");
        connection_registry.register_entry(jid.clone(), entry);

        let teardown = tokio::spawn({
            let user_local_claims = Arc::clone(&user_local_claims);
            async move { user_local_claims.demote_all_on_self_fence().await }
        });
        let request = force_detach_rx.recv().await.expect("relay request");
        assert_eq!(request.reason, ForceDetachReason::NodeSelfFenced);
        request
            .ack
            .send(ForceDetachOutcome::NotPersisted)
            .expect("confirmed remote socket retirement");
        assert!(teardown.await.expect("terminal teardown task"));
        assert!(
            !connection_registry.is_connected(&jid),
            "owner-side proxy state must not pin this node's readiness on a remote relay"
        );
    }

    #[tokio::test]
    async fn closed_remote_mirror_control_queue_cannot_claim_terminal_teardown_without_compensation(
    ) {
        let user_local_claims = UserLocalClaims::new();
        let connection_registry = Arc::new(ConnectionRegistry::new());
        user_local_claims.wire_connection_registry(Arc::clone(&connection_registry));

        let jid: FullJid = "remote-mirror@example.com/closed"
            .parse()
            .expect("valid full JID");
        let (outbound_tx, _outbound_rx) = tokio::sync::mpsc::channel(4);
        let entry = ConnectionEntry::new_remote_mirror(outbound_tx);
        drop(
            entry
                .take_force_detach_rx()
                .expect("mirror forwarder owns force-detach receiver"),
        );
        connection_registry.register_entry(jid.clone(), entry);

        assert!(
            !user_local_claims.demote_all_on_self_fence().await,
            "without a relay compensation bridge, a closed queue must keep terminal teardown incomplete"
        );
        assert!(
            connection_registry.is_connected(&jid),
            "uncertain physical-socket retirement must retain proxy state for retry"
        );
    }

    #[tokio::test]
    async fn user_self_fence_bulk_demotes_every_actor_in_one_registry_operation() {
        let user_local_claims = UserLocalClaims::new();
        let registry = spawn_user_registry();
        user_local_claims.wire(registry.clone());
        for username in ["one", "two", "three"] {
            registry
                .ask(waddle_xmpp::registry::GetOrCreateUser {
                    bare_jid: user_jid(username),
                })
                .await
                .expect("create user actor");
        }
        assert_eq!(user_local_claims.owned().await.len(), 3);

        assert!(user_local_claims.demote_all_on_self_fence().await);

        assert!(
            user_local_claims.owned().await.is_empty(),
            "terminal bulk demotion must atomically drain the user registry"
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
    async fn user_demote_detaches_exact_stale_actor_entry_not_same_jid_replacement() {
        let user_local_claims = UserLocalClaims::new();
        let registry = spawn_user_registry();
        let connection_registry = Arc::new(ConnectionRegistry::new());
        user_local_claims.wire(registry.clone());
        user_local_claims.wire_connection_registry(Arc::clone(&connection_registry));

        let jid: FullJid = "replacement@example.com/phone"
            .parse()
            .expect("valid full JID");
        let (old_tx, _old_outbound_rx) = tokio::sync::mpsc::channel(4);
        let old_owner = connection_registry.register(jid.clone(), old_tx);
        let old_entry = connection_registry
            .entry_if_owner(&jid, &old_owner)
            .expect("old entry");
        let mut old_force_detach_rx = old_entry
            .take_force_detach_rx()
            .expect("old connection owns force-detach receiver");
        registry
            .ask(waddle_xmpp::registry::RegisterUserResource {
                jid: jid.clone(),
                entry: old_entry,
            })
            .await
            .expect("register old actor resource");

        let (new_tx, _new_outbound_rx) = tokio::sync::mpsc::channel(4);
        let new_owner = connection_registry.register(jid.clone(), new_tx);
        let new_entry = connection_registry
            .entry_if_owner(&jid, &new_owner)
            .expect("replacement entry");
        let mut new_force_detach_rx = new_entry
            .take_force_detach_rx()
            .expect("replacement owns force-detach receiver");
        let old_detach = tokio::spawn(async move {
            let request = old_force_detach_rx
                .recv()
                .await
                .expect("old exact entry receives force-detach");
            assert_eq!(request.reason, ForceDetachReason::OwnershipLost);
            let _ = request.ack.send(ForceDetachOutcome::NotPersisted);
        });

        user_local_claims
            .demote(&Entity::new(
                EntityType::UserActor,
                jid.to_bare().to_string(),
            ))
            .await;
        old_detach.await.expect("old detach task");

        assert!(matches!(
            new_force_detach_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert!(
            connection_registry
                .entry_if_owner(&jid, &new_owner)
                .is_some(),
            "reconciling the stale actor must not detach its same-JID replacement"
        );
    }

    #[tokio::test]
    async fn user_self_fence_detaches_actor_only_and_current_same_jid_incarnations() {
        let user_local_claims = UserLocalClaims::new();
        let registry = spawn_user_registry();
        let connection_registry = Arc::new(ConnectionRegistry::new());
        user_local_claims.wire(registry.clone());
        user_local_claims.wire_connection_registry(Arc::clone(&connection_registry));

        let jid: FullJid = "fence-race@example.com/phone"
            .parse()
            .expect("valid full JID");
        let (old_tx, _old_outbound_rx) = tokio::sync::mpsc::channel(4);
        let old_owner = connection_registry.register(jid.clone(), old_tx);
        let old_entry = connection_registry
            .entry_if_owner(&jid, &old_owner)
            .expect("old entry");
        let mut old_rx = old_entry
            .take_force_detach_rx()
            .expect("old connection receiver");
        registry
            .ask(waddle_xmpp::registry::RegisterUserResource {
                jid: jid.clone(),
                entry: old_entry,
            })
            .await
            .expect("register old actor resource");

        let (new_tx, _new_outbound_rx) = tokio::sync::mpsc::channel(4);
        let new_owner = connection_registry.register(jid.clone(), new_tx);
        let new_entry = connection_registry
            .entry_if_owner(&jid, &new_owner)
            .expect("new entry");
        let mut new_rx = new_entry
            .take_force_detach_rx()
            .expect("new connection receiver");
        let old_detach = tokio::spawn(async move {
            let request = old_rx.recv().await.expect("old detach");
            assert_eq!(request.reason, ForceDetachReason::NodeSelfFenced);
            let _ = request.ack.send(ForceDetachOutcome::NotPersisted);
        });
        let new_detach = tokio::spawn(async move {
            let request = new_rx.recv().await.expect("new detach");
            assert_eq!(request.reason, ForceDetachReason::NodeSelfFenced);
            let _ = request.ack.send(ForceDetachOutcome::NotPersisted);
        });

        assert!(user_local_claims.demote_all_on_self_fence().await);
        old_detach.await.expect("old detach task");
        new_detach.await.expect("new detach task");
        assert!(!connection_registry.is_connected(&jid));
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
    async fn room_self_fence_bulk_demotes_every_actor_in_one_registry_operation() {
        let room_local_claims = RoomLocalClaims::new();
        let registry = waddle_xmpp::muc::RoomRegistry::spawn(
            "muc.example.com".to_string(),
            test_occupant_id_secret(),
            None,
        );
        room_local_claims.wire(registry.clone());
        for room in ["one", "two", "three"] {
            let room_jid: jid::BareJid = format!("{room}@muc.example.com")
                .parse()
                .expect("valid room JID");
            registry
                .get_or_create_room(
                    room_jid,
                    format!("waddle-{room}"),
                    format!("channel-{room}"),
                    waddle_xmpp::muc::RoomConfig::default(),
                )
                .await
                .expect("create room");
        }
        assert_eq!(room_local_claims.owned().await.len(), 3);

        assert!(room_local_claims.demote_all_on_self_fence().await);

        assert!(
            room_local_claims.owned().await.is_empty(),
            "terminal bulk demotion must atomically drain the room registry"
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

    impl waddle_xmpp::muc::MucDurableStore for RecordingDurableStore {
        fn load_room_state<'a>(
            &'a self,
            _room_jid: &'a jid::BareJid,
        ) -> waddle_xmpp::muc::MucDurableFuture<'a, Option<waddle_xmpp::muc::DurableRoomState>>
        {
            Box::pin(async { Ok(None) })
        }

        fn save_config<'a>(
            &'a self,
            _room_jid: &'a jid::BareJid,
            _waddle_id: &'a str,
            _channel_id: &'a str,
            _config: &'a waddle_xmpp::muc::RoomConfig,
        ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
            Box::pin(async move {
                self.started.notify_one();
                tokio::time::sleep(Duration::from_millis(150)).await;
                self.persisted
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        }

        fn save_subject<'a>(
            &'a self,
            _room_jid: &'a jid::BareJid,
            _subject: Option<&'a waddle_xmpp::muc::SubjectState>,
        ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }

        fn save_affiliation<'a>(
            &'a self,
            _room_jid: &'a jid::BareJid,
            _entry: &'a waddle_xmpp::muc::affiliation::AffiliationEntry,
        ) -> waddle_xmpp::muc::MucDurableFuture<'a, ()> {
            Box::pin(async { Ok(()) })
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
