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

use async_trait::async_trait;
use waddle_xmpp::ownership::{Entity, EntityType};
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;

use super::self_fence::LocallyClaimedEntities;

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
    fn owned(&self) -> Vec<Entity> {
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
        }
    }

    #[test]
    fn unwired_instance_owns_nothing() {
        let local_claims = SmSessionLocalClaims::new();
        assert!(local_claims.owned().is_empty());
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

        let owned = local_claims.owned();
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
        assert_eq!(local_claims.owned().len(), 1);

        let entity = Entity::new(EntityType::SmSession, "stream-b".to_string());
        local_claims.demote(&entity).await;
        assert!(local_claims.owned().is_empty());

        // A different entity type for the same id must be a no-op.
        registry
            .store_session(test_session("stream-c", "carol@example.com/res"))
            .await
            .expect("store session");
        let foreign_type_entity = Entity::new(EntityType::UserActor, "stream-c".to_string());
        local_claims.demote(&foreign_type_entity).await;
        assert_eq!(
            local_claims.owned().len(),
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
}
