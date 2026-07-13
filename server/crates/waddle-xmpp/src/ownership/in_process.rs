//! Trivial single-node [`ClaimStore`] (ADR-0017 Phase 3 Slice 1, Q2).
//!
//! Used whenever `clustering.enabled` is false (the default) — including
//! plain single-node SQLite deployments, which never touch Postgres at all.
//! There is only one node, so contention with *another* node is
//! structurally impossible — but contention between two *connections on
//! this same node* (e.g. two attempts to claim the same SM session) is
//! real, and this store enforces it exactly as `PostgresClaimStore` does:
//! `acquire` succeeds iff the entity is currently unclaimed, returning
//! [`ClaimError::AlreadyClaimed`] otherwise. The contract is identical
//! across both `ClaimStore` implementations — this store is not a
//! permissive stand-in that happens to work differently in the
//! uncontested case. `heartbeat`/demotion-reconciliation (a separate,
//! per-node concern — see the trait-level doc on [`super::ClaimStore`])
//! are simply not needed here, since there is only one node's liveness to
//! track and it never expires itself.
//!
//! This still tracks real per-entity state (a claim map + epoch counter),
//! rather than being a pure no-op, so callers exercise the same
//! acquire → fence → release lifecycle a Postgres-backed store would
//! enforce.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use super::{
    ClaimEpoch, ClaimError, ClaimStore, Entity, NodeIdentity, ResumeIdentityProof, StalePredicate,
};

/// A held claim's owner + current fencing epoch. `owner` is needed so
/// [`InProcessClaimStore::ensure_claimed`] (FIX 1) can distinguish a
/// self-reacquire (`owner == me`) from a genuine conflict — bare `ClaimEpoch`
/// storage (the pre-FIX-1 shape) had no way to answer "who holds this."
#[derive(Clone)]
struct ClaimRecord {
    owner: NodeIdentity,
    epoch: ClaimEpoch,
}

#[derive(Default)]
struct ClaimState {
    claims: HashMap<Entity, ClaimRecord>,
    next_generation: i64,
}

impl ClaimState {
    fn allocate_generation(&mut self) -> Result<ClaimEpoch, ClaimError> {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.checked_add(1).ok_or_else(|| {
            ClaimError::Backend("in-process claim generation exhausted".to_string())
        })?;
        Ok(ClaimEpoch(generation))
    }
}

/// In-memory claim bookkeeping for the single-node case.
#[derive(Default)]
pub struct InProcessClaimStore {
    state: Mutex<ClaimState>,
}

impl InProcessClaimStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ClaimState>, ClaimError> {
        self.state.lock().map_err(|_| ClaimError::Poisoned)
    }
}

#[async_trait]
impl ClaimStore for InProcessClaimStore {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        // No backing schema — purely in-memory.
        Ok(())
    }

    async fn acquire(&self, entity: &Entity, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        if !me.is_active() {
            return Err(ClaimError::AuthorityDisabled);
        }
        let mut state = self.lock()?;
        // Same contract as `PostgresClaimStore::acquire` (INSERT ... ON
        // CONFLICT DO NOTHING): a fresh claim only succeeds when the
        // entity is currently unclaimed. A second acquire against an
        // already-held entity loses the race, exactly as a second node
        // would against the real CAS — this is same-node contention
        // (e.g. two connections racing to claim the same SM session),
        // which is real and must be enforced, not idempotent-Ok'd away.
        if state.claims.contains_key(entity) {
            return Err(ClaimError::AlreadyClaimed);
        }
        let epoch = state.allocate_generation()?;
        state.claims.insert(
            entity.clone(),
            ClaimRecord {
                owner: me.clone(),
                epoch,
            },
        );
        Ok(epoch)
    }

    async fn ensure_claimed(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        if !me.is_active() {
            return Err(ClaimError::AuthorityDisabled);
        }
        // FIX 1: same observable contract as `PostgresClaimStore::ensure_claimed`
        // — a fresh acquire, or (on conflict) a self-reacquire iff the
        // existing row's owner is exactly `me`. Implemented directly against
        // the lock rather than by calling `acquire` and re-locking on
        // conflict, since this store already holds everything it needs
        // under one lock acquisition.
        let mut state = self.lock()?;
        match state.claims.get(entity) {
            None => {
                let epoch = state.allocate_generation()?;
                state.claims.insert(
                    entity.clone(),
                    ClaimRecord {
                        owner: me.clone(),
                        epoch,
                    },
                );
                Ok(epoch)
            }
            Some(record) if record.owner == *me => Ok(record.epoch),
            Some(_) => Err(ClaimError::AlreadyClaimed),
        }
    }

    async fn steal_stale(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        _staleness: StalePredicate,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        if !me.is_active() {
            return Err(ClaimError::AuthorityDisabled);
        }
        // Real epoch-fenced CAS, mirroring `PostgresClaimStore::steal_stale`'s
        // `WHERE entity=$e AND claim_epoch=$observed` gate exactly (ADR-0017
        // Phase 3 Slice 6 fix: the previous unconditional-overwrite shape
        // ignored `observed` entirely, so it could never lose a race — which
        // made this store useless for exercising the two-node claim-steal
        // races Slice 6's dedicated tests simulate in-process, sharing one
        // `InProcessClaimStore` between two distinct `NodeIdentity` values.
        // There is no single-node owner-staleness concept to check
        // separately (no `clustering_nodes`-equivalent liveness table here),
        // so `_staleness` is accepted but not consulted — same simplification
        // the "trivial single node" module doc already makes for every other
        // method.
        let mut state = self.lock()?;
        match state.claims.get(entity) {
            Some(record) if record.epoch == observed => {
                let new_epoch = state.allocate_generation()?;
                state.claims.insert(
                    entity.clone(),
                    ClaimRecord {
                        owner: me.clone(),
                        epoch: new_epoch,
                    },
                );
                Ok(new_epoch)
            }
            // Stale observed epoch, or no claim exists at all to steal —
            // both are `Conflict`, exactly like the Postgres CAS affecting
            // zero rows.
            _ => Err(ClaimError::Conflict),
        }
    }

    async fn steal_for_resume(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        _witness: ResumeIdentityProof,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        // Same real epoch-fenced CAS as `steal_stale` — the witness only
        // gates *callers* (only `verify_resume_identity` can mint one), not
        // this store's internal bookkeeping, exactly matching
        // `PostgresClaimStore::steal_for_resume`'s own "no staleness
        // predicate at all" shape.
        self.steal_stale(entity, observed, StalePredicate::OwnerStale, me)
            .await
    }

    async fn current_claim(
        &self,
        entity: &Entity,
    ) -> Result<Option<super::ClaimSnapshot>, ClaimError> {
        let state = self.lock()?;
        Ok(state.claims.get(entity).map(|record| super::ClaimSnapshot {
            owner: record.owner.clone(),
            claim_epoch: record.epoch,
            // No node-liveness table in this single-node store (see this
            // type's module doc) — always fresh.
            owner_lease_fresh: true,
        }))
    }

    async fn fence(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<bool, ClaimError> {
        let state = self.lock()?;
        Ok(
            matches!(state.claims.get(entity), Some(record) if record.owner == *me && record.epoch == mine),
        )
    }

    async fn release(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<(), ClaimError> {
        let mut state = self.lock()?;
        if matches!(state.claims.get(entity), Some(record) if record.owner == *me && record.epoch == mine)
        {
            state.claims.remove(entity);
        }
        Ok(())
    }

    async fn release_exact(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<crate::ownership::ExactReleaseOutcome, ClaimError> {
        let mut state = self.lock()?;
        if matches!(state.claims.get(entity), Some(record) if record.owner == *me && record.epoch == mine)
        {
            state.claims.remove(entity);
            Ok(crate::ownership::ExactReleaseOutcome::Released)
        } else {
            Ok(crate::ownership::ExactReleaseOutcome::NotOwned)
        }
    }

    async fn release_many(&self, entities: &[Entity], me: &NodeIdentity) -> Result<(), ClaimError> {
        // Claim-epoch-blind by the trait contract, but still exact-owner
        // gated so an old process incarnation cannot drain claims acquired
        // by a replacement sharing the same stable node id.
        let mut state = self.lock()?;
        for entity in entities {
            if matches!(state.claims.get(entity), Some(record) if record.owner == *me) {
                state.claims.remove(entity);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::EntityType;

    fn entity(id: &str) -> Entity {
        Entity::new(EntityType::SmSession, id)
    }

    fn me() -> NodeIdentity {
        NodeIdentity::local()
    }

    #[tokio::test]
    async fn acquire_succeeds_once_then_conflicts_same_node() {
        // Same-node contention is real and enforced: a second acquire
        // against a still-held entity must lose, identically to
        // `PostgresClaimStore::acquire_succeeds_once_then_conflicts`
        // (`waddle-server/src/clustering/claims.rs`) — the contract is
        // identical across both `ClaimStore` implementations.
        let store = InProcessClaimStore::new();
        let e = entity("stream-1");
        let first = store.acquire(&e, &me()).await.expect("first acquire");
        assert_eq!(first, ClaimEpoch(0));

        let err = store
            .acquire(&e, &me())
            .await
            .expect_err("second acquire against a still-held entity loses the race");
        assert!(matches!(err, ClaimError::AlreadyClaimed));

        // Once released, the entity is claimable again under a generation
        // that can never alias the deleted claim.
        store.release(&e, &me(), first).await.expect("release");
        let reacquired = store
            .acquire(&e, &me())
            .await
            .expect("re-acquire after release");
        assert!(reacquired.0 > first.0);
        assert_eq!(
            store
                .release_exact(&e, &me(), first)
                .await
                .expect("stale exact release"),
            crate::ownership::ExactReleaseOutcome::NotOwned
        );
        assert!(store
            .fence(&e, &me(), reacquired)
            .await
            .expect("recreated generation remains held"));
    }

    #[tokio::test]
    async fn generations_are_monotonic_across_entities_and_steals() {
        let store = InProcessClaimStore::new();
        let a = entity("generation-a");
        let b = entity("generation-b");
        let a0 = store.acquire(&a, &me()).await.expect("acquire a");
        let b0 = store.acquire(&b, &me()).await.expect("acquire b");
        let a1 = store
            .steal_stale(&a, a0, StalePredicate::OwnerStale, &me())
            .await
            .expect("steal a");

        assert!(a0.0 < b0.0 && b0.0 < a1.0);
    }

    #[tokio::test]
    async fn release_is_idempotent_and_exact_release_reports_ownership() {
        let store = InProcessClaimStore::new();
        let e = entity("stream-1");
        let epoch0 = store.acquire(&e, &me()).await.expect("acquire");
        let epoch1 = store
            .steal_stale(&e, epoch0, StalePredicate::OwnerStale, &me())
            .await
            .expect("steal bumps the epoch");

        // Releasing under the superseded epoch must not drop the claim.
        store
            .release(&e, &me(), epoch0)
            .await
            .expect("stale release is an idempotent no-op");
        let err = store
            .acquire(&e, &me())
            .await
            .expect_err("claim survives a stale-epoch release");
        assert!(matches!(err, ClaimError::AlreadyClaimed));

        // Releasing under the current epoch drops it; a repeat is idempotent.
        store.release(&e, &me(), epoch1).await.expect("release");
        store
            .release(&e, &me(), epoch1)
            .await
            .expect("releasing an absent claim is idempotent");
        store
            .acquire(&e, &me())
            .await
            .expect("entity claimable again after a current-epoch release");

        assert_eq!(
            store
                .release_exact(&e, &me(), ClaimEpoch(99))
                .await
                .expect("exact release"),
            crate::ownership::ExactReleaseOutcome::NotOwned
        );
    }

    #[tokio::test]
    async fn same_node_id_from_another_incarnation_cannot_fence_or_release() {
        let store = InProcessClaimStore::new();
        let entity = entity("incarnation-fence");
        let owner = NodeIdentity::new("same-node", "epoch-a");
        let replacement = NodeIdentity::new("same-node", "epoch-b");
        let claim_epoch = store.acquire(&entity, &owner).await.expect("acquire");

        assert!(!store
            .fence(&entity, &replacement, claim_epoch)
            .await
            .expect("fence"));
        assert_eq!(
            store
                .release_exact(&entity, &replacement, claim_epoch)
                .await
                .expect("exact release"),
            crate::ownership::ExactReleaseOutcome::NotOwned
        );
        assert!(store
            .fence(&entity, &owner, claim_epoch)
            .await
            .expect("original owner remains"));
    }

    #[tokio::test]
    async fn ensure_claimed_is_a_fresh_acquire_when_unclaimed() {
        let store = InProcessClaimStore::new();
        let e = entity("stream-1");
        let epoch = store
            .ensure_claimed(&e, &me())
            .await
            .expect("ensure_claimed on an unclaimed entity acquires fresh");
        assert_eq!(epoch, ClaimEpoch(0));
    }

    #[tokio::test]
    async fn ensure_claimed_is_idempotent_for_the_same_node_and_epoch() {
        // FIX 1: a second `ensure_claimed` call by the exact same node
        // identity that already holds the claim must observe the existing
        // epoch, not `AlreadyClaimed` — the whole point of the fix (closes
        // the concurrent-first-write spurious-conflict case).
        let store = InProcessClaimStore::new();
        let e = entity("stream-1");
        let first = store.ensure_claimed(&e, &me()).await.expect("first");
        let second = store
            .ensure_claimed(&e, &me())
            .await
            .expect("self-reacquire must not error");
        assert_eq!(first, second);
        assert_eq!(second, ClaimEpoch(0));
    }

    #[tokio::test]
    async fn ensure_claimed_rejects_a_foreign_owner() {
        let store = InProcessClaimStore::new();
        let e = entity("stream-1");
        store.acquire(&e, &me()).await.expect("acquire under me()");

        let foreign = NodeIdentity::new("other-node", "other-epoch");
        let err = store
            .ensure_claimed(&e, &foreign)
            .await
            .expect_err("a different node/epoch must not self-reacquire");
        assert!(matches!(err, ClaimError::AlreadyClaimed));
    }

    #[tokio::test]
    async fn steal_stale_bumps_the_epoch() {
        let store = InProcessClaimStore::new();
        let e = entity("stream-1");
        let epoch0 = store.acquire(&e, &me()).await.expect("acquire");
        let epoch1 = store
            .steal_stale(&e, epoch0, StalePredicate::OwnerStale, &me())
            .await
            .expect("steal");
        assert_eq!(epoch1, ClaimEpoch(1));
    }

    #[tokio::test]
    async fn steal_for_resume_requires_a_minted_proof() {
        let store = InProcessClaimStore::new();
        let e = entity("stream-1");
        let epoch0 = store.acquire(&e, &me()).await.expect("acquire");
        let jid: jid::BareJid = "alice@example.com".parse().expect("valid jid");
        let proof = crate::ownership::verify_resume_identity(&jid, &jid).expect("identity match");
        let epoch1 = store
            .steal_for_resume(&e, epoch0, proof, &me())
            .await
            .expect("steal_for_resume");
        assert_eq!(epoch1, ClaimEpoch(1));
    }

    #[tokio::test]
    async fn fence_reflects_current_epoch_only() {
        let store = InProcessClaimStore::new();
        let e = entity("stream-1");
        let epoch0 = store.acquire(&e, &me()).await.expect("acquire");
        assert!(store.fence(&e, &me(), epoch0).await.expect("fence"));
        assert!(!store
            .fence(&e, &me(), ClaimEpoch(99))
            .await
            .expect("fence wrong epoch"));

        let epoch1 = store
            .steal_stale(&e, epoch0, StalePredicate::OwnerStale, &me())
            .await
            .expect("steal");
        assert!(!store
            .fence(&e, &me(), epoch0)
            .await
            .expect("fence stale epoch after steal"));
        assert!(store.fence(&e, &me(), epoch1).await.expect("fence current"));
    }

    #[tokio::test]
    async fn release_clears_the_claim() {
        let store = InProcessClaimStore::new();
        let e = entity("stream-1");
        let epoch0 = store.acquire(&e, &me()).await.expect("acquire");
        store.release(&e, &me(), epoch0).await.expect("release");
        assert!(!store
            .fence(&e, &me(), epoch0)
            .await
            .expect("fence after release"));
    }

    #[tokio::test]
    async fn steal_stale_with_a_stale_observed_epoch_loses_the_race() {
        // ADR-0017 Phase 3 Slice 6 fix: this store must actually lose a CAS
        // race, not unconditionally overwrite — otherwise it cannot stand in
        // for a two-node claim-steal simulation in a dedicated test.
        let store = InProcessClaimStore::new();
        let e = entity("stream-1");
        store.acquire(&e, &me()).await.expect("acquire");
        let err = store
            .steal_stale(&e, ClaimEpoch(41), StalePredicate::OwnerStale, &me())
            .await
            .expect_err("wrong observed epoch loses");
        assert!(matches!(err, ClaimError::Conflict));
    }

    #[tokio::test]
    async fn steal_stale_against_a_nonexistent_claim_conflicts() {
        let store = InProcessClaimStore::new();
        let e = entity("stream-1");
        let err = store
            .steal_stale(&e, ClaimEpoch(0), StalePredicate::OwnerStale, &me())
            .await
            .expect_err("nothing to steal");
        assert!(matches!(err, ClaimError::Conflict));
    }

    #[tokio::test]
    async fn steal_for_resume_between_two_node_identities_sharing_one_store() {
        // The in-process "two-registry simulation" idiom Slice 6's dedicated
        // tests use: two distinct `NodeIdentity` values racing
        // `steal_for_resume` against one shared store, standing in for two
        // nodes sharing one Postgres `clustering_claims` table.
        let store = InProcessClaimStore::new();
        let e = entity("stream-1");
        let owner = NodeIdentity::new("node-a", "epoch-a");
        let epoch0 = store.acquire(&e, &owner).await.expect("acquire");

        let stealer = NodeIdentity::new("node-b", "epoch-b");
        let jid: jid::BareJid = "alice@example.com".parse().expect("valid jid");
        let proof = crate::ownership::verify_resume_identity(&jid, &jid).expect("identity match");
        let epoch1 = store
            .steal_for_resume(&e, epoch0, proof, &stealer)
            .await
            .expect("consent CAS steals from a live-but-consenting owner");
        assert_eq!(epoch1, ClaimEpoch(1));

        let snapshot = store
            .current_claim(&e)
            .await
            .expect("current_claim")
            .expect("claim exists");
        assert_eq!(snapshot.owner, stealer);
        assert_eq!(snapshot.claim_epoch, ClaimEpoch(1));

        // The original owner retrying against its now-stale observed epoch
        // loses cleanly.
        let jid2: jid::BareJid = "alice@example.com".parse().expect("valid jid");
        let proof2 =
            crate::ownership::verify_resume_identity(&jid2, &jid2).expect("identity match");
        let err = store
            .steal_for_resume(&e, epoch0, proof2, &owner)
            .await
            .expect_err("stale epoch loses");
        assert!(matches!(err, ClaimError::Conflict));
    }

    #[tokio::test]
    async fn current_claim_is_none_for_an_unclaimed_entity() {
        let store = InProcessClaimStore::new();
        let e = entity("stream-1");
        assert!(store
            .current_claim(&e)
            .await
            .expect("current_claim")
            .is_none());
    }

    #[tokio::test]
    async fn release_many_clears_every_listed_entity() {
        let store = InProcessClaimStore::new();
        let a = entity("stream-a");
        let b = entity("stream-b");
        let a_epoch = store.acquire(&a, &me()).await.expect("acquire a");
        let b_epoch = store.acquire(&b, &me()).await.expect("acquire b");

        store
            .release_many(&[a.clone(), b.clone()], &me())
            .await
            .expect("release_many");

        assert!(!store.fence(&a, &me(), a_epoch).await.expect("fence a"));
        assert!(!store.fence(&b, &me(), b_epoch).await.expect("fence b"));
    }

    #[tokio::test]
    async fn release_many_only_removes_claims_owned_by_the_exact_incarnation() {
        let store = InProcessClaimStore::new();
        let old_identity = NodeIdentity::new("stable-node", "epoch-a");
        let current_identity = NodeIdentity::new("stable-node", "epoch-b");
        let old_entity = entity("old-incarnation");
        let current_entity = entity("current-incarnation");
        let old_epoch = store
            .acquire(&old_entity, &old_identity)
            .await
            .expect("old incarnation acquires claim");
        let current_epoch = store
            .acquire(&current_entity, &current_identity)
            .await
            .expect("current incarnation acquires claim");

        store
            .release_many(
                &[old_entity.clone(), current_entity.clone()],
                &current_identity,
            )
            .await
            .expect("current incarnation drains its claims");

        assert!(store
            .fence(&old_entity, &old_identity, old_epoch)
            .await
            .expect("old claim remains fenced"));
        assert!(!store
            .fence(&current_entity, &current_identity, current_epoch)
            .await
            .expect("current claim was released"));
    }
}
