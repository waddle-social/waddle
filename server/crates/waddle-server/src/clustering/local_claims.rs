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
//! **Scope note**: `owned()` only ever reports `EntityType::SmSession`
//! entities. `UserActor`/`RoomActor` claim acquisition is out of this
//! slice's Files list (the phase plan frames it as "Slices 5-7"), so this
//! impl does not — and must not — fabricate wiring for either. This keeps
//! the steal-intent veto scan (`self_fence::run_node_lease`) exactly as
//! vacuous in production as it was under `NoLocallyClaimedEntities`:
//! steal-intents never apply to `SmSession` claims at all (Slice 3 rule 1),
//! so `owner_steal_intents` never returns one of the entities this impl's
//! `owned()` reports, regardless.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use jid::BareJid;
use waddle_xmpp::muc::room_actor::HealthCheck;
use waddle_xmpp::muc::RoomRegistry;
use waddle_xmpp::ownership::{Entity, EntityType};
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;

use super::self_fence::LocallyClaimedEntities;

/// Bound on the health-ask this impl issues against a locally-claimed
/// room's `RoomActor` (ADR-0017 Phase 3 Slice 7's `RoomActor` counterpart
/// of the `UserActor` owner-veto path). Chosen well below the steal-intent
/// TTL / heartbeat interval so a genuinely wedged room is detected and
/// hard-killed within one veto-scan tick, never straddling two.
const ROOM_HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// See the module doc for the construction-order rationale.
pub struct SmSessionLocalClaims {
    registry: OnceLock<Arc<InMemorySmSessionRegistry>>,
}

impl SmSessionLocalClaims {
    /// Construct empty. See the module doc's construction-order note.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            registry: OnceLock::new(),
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
    async fn hydrate_reclaimed(&self, entities: &[(Entity, waddle_xmpp::ownership::ClaimEpoch)]) {
        let Some(registry) = self.registry.get() else {
            return;
        };
        if let Err(error) = registry.hydrate_reclaimed(entities).await {
            tracing::warn!(
                %error,
                "SmSessionLocalClaims::hydrate_reclaimed: registry hydrate_reclaimed failed"
            );
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

/// Dispatches [`LocallyClaimedEntities`] calls across `SmSession` and
/// `RoomActor` claims by `entity.entity_type` (ADR-0017 Phase 3 Slice 7):
/// `run_node_lease` takes exactly one `Arc<dyn LocallyClaimedEntities>`
/// handle, but Slices 5 and 7 each contribute their own concrete
/// implementation over a different owned-entity universe. Combining them
/// here, rather than widening either concrete type to know about the
/// other's entities, keeps `SmSessionLocalClaims`/`RoomLocalClaims`
/// independently testable (as both already are).
pub struct CombinedLocalClaims {
    sm: Arc<SmSessionLocalClaims>,
    room: Arc<RoomLocalClaims>,
}

impl CombinedLocalClaims {
    pub fn new(sm: Arc<SmSessionLocalClaims>, room: Arc<RoomLocalClaims>) -> Arc<Self> {
        Arc::new(Self { sm, room })
    }
}

#[async_trait]
impl LocallyClaimedEntities for CombinedLocalClaims {
    async fn owned(&self) -> Vec<Entity> {
        let mut owned = self.sm.owned().await;
        owned.extend(self.room.owned().await);
        owned
    }

    async fn demote(&self, entity: &Entity) {
        match entity.entity_type {
            EntityType::SmSession => self.sm.demote(entity).await,
            EntityType::RoomActor => self.room.demote(entity).await,
            // No `UserActor`-backed `LocallyClaimedEntities` implementor
            // exists yet (deviation 21/34's Phase-4 carry-forward) — a
            // `UserActor` entity never appears in `owned()`, so this arm
            // is defensive only.
            EntityType::UserActor => {}
        }
    }

    async fn health_check(&self, entity: &Entity) -> bool {
        match entity.entity_type {
            EntityType::SmSession => self.sm.health_check(entity).await,
            EntityType::RoomActor => self.room.health_check(entity).await,
            EntityType::UserActor => true,
        }
    }

    async fn hydrate_reclaimed(&self, entities: &[(Entity, waddle_xmpp::ownership::ClaimEpoch)]) {
        let (sm_entities, rest): (Vec<_>, Vec<_>) = entities
            .iter()
            .cloned()
            .partition(|(entity, _)| entity.entity_type == EntityType::SmSession);
        if !sm_entities.is_empty() {
            self.sm.hydrate_reclaimed(&sm_entities).await;
        }
        // `RoomLocalClaims`/`UserActor` have no reclaim-hydration
        // consumer yet (no production caller acquires a `RoomActor`
        // claim via the reclaim-sweep path this slice — only via
        // `GetOrCreateRoom`'s own `ensure_claimed`/`steal_stale`, which
        // already restores through `RestoreDurableRoomState`), so `rest`
        // is intentionally not forwarded anywhere; named here so a
        // future contributor adding that consumer has an obvious seam.
        let _ = rest;
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
    /// [`RoomLocalClaims::seal_before_release`]'s real barrier.
    /// `UserActor` keeps the default `true` too — defensive only, no
    /// production `UserActor` claim-acquisition call site exists yet
    /// (deviation 21/34).
    async fn seal_before_release(&self, entity: &Entity) -> bool {
        match entity.entity_type {
            EntityType::SmSession | EntityType::UserActor => true,
            EntityType::RoomActor => self.room.seal_before_release(entity).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
