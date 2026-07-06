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

/// In-memory claim bookkeeping for the single-node case.
#[derive(Default)]
pub struct InProcessClaimStore {
    claims: Mutex<HashMap<Entity, ClaimRecord>>,
}

impl InProcessClaimStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<Entity, ClaimRecord>>, ClaimError> {
        self.claims.lock().map_err(|_| ClaimError::Poisoned)
    }
}

#[async_trait]
impl ClaimStore for InProcessClaimStore {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        // No backing schema — purely in-memory.
        Ok(())
    }

    async fn acquire(&self, entity: &Entity, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        let mut claims = self.lock()?;
        // Same contract as `PostgresClaimStore::acquire` (INSERT ... ON
        // CONFLICT DO NOTHING): a fresh claim only succeeds when the
        // entity is currently unclaimed. A second acquire against an
        // already-held entity loses the race, exactly as a second node
        // would against the real CAS — this is same-node contention
        // (e.g. two connections racing to claim the same SM session),
        // which is real and must be enforced, not idempotent-Ok'd away.
        if claims.contains_key(entity) {
            return Err(ClaimError::AlreadyClaimed);
        }
        claims.insert(
            entity.clone(),
            ClaimRecord {
                owner: me.clone(),
                epoch: ClaimEpoch(0),
            },
        );
        Ok(ClaimEpoch(0))
    }

    async fn ensure_claimed(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        // FIX 1: same observable contract as `PostgresClaimStore::ensure_claimed`
        // — a fresh acquire, or (on conflict) a self-reacquire iff the
        // existing row's owner is exactly `me`. Implemented directly against
        // the lock rather than by calling `acquire` and re-locking on
        // conflict, since this store already holds everything it needs
        // under one lock acquisition.
        let mut claims = self.lock()?;
        match claims.get(entity) {
            None => {
                claims.insert(
                    entity.clone(),
                    ClaimRecord {
                        owner: me.clone(),
                        epoch: ClaimEpoch(0),
                    },
                );
                Ok(ClaimEpoch(0))
            }
            Some(record) if record.owner == *me => Ok(record.epoch),
            Some(_) => Err(ClaimError::AlreadyClaimed),
        }
    }

    async fn steal_stale(
        &self,
        entity: &Entity,
        _observed: ClaimEpoch,
        _staleness: StalePredicate,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        // Single node: there is no other node to steal from, so this
        // trivially succeeds by bumping the epoch (mirroring what a fresh
        // acquire-after-steal would observe on the real CAS).
        let mut claims = self.lock()?;
        let entry = claims.entry(entity.clone()).or_insert(ClaimRecord {
            owner: me.clone(),
            epoch: ClaimEpoch(0),
        });
        entry.epoch.0 += 1;
        entry.owner = me.clone();
        Ok(entry.epoch)
    }

    async fn steal_for_resume(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        _witness: ResumeIdentityProof,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        // Same trivial single-node semantics as `steal_stale`; the witness
        // only gates *callers*, not this store's internal bookkeeping.
        self.steal_stale(entity, observed, StalePredicate::OwnerStale, me)
            .await
    }

    async fn fence(
        &self,
        entity: &Entity,
        _me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<bool, ClaimError> {
        let claims = self.lock()?;
        Ok(claims.get(entity).map(|record| record.epoch) == Some(mine))
    }

    async fn release(
        &self,
        entity: &Entity,
        _me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<(), ClaimError> {
        let mut claims = self.lock()?;
        // Epoch-gated exactly like `PostgresClaimStore`'s CAS: a losing
        // epoch is a no-op (the claim was re-issued since the caller
        // observed `mine`), and releasing an absent claim is idempotent.
        if claims.get(entity).map(|record| record.epoch) == Some(mine) {
            claims.remove(entity);
        }
        Ok(())
    }

    async fn release_many(
        &self,
        entities: &[Entity],
        _me: &NodeIdentity,
    ) -> Result<(), ClaimError> {
        // Claim-epoch-blind by the trait contract (the node-identity gate
        // is trivially satisfied here: this store *is* the one node).
        let mut claims = self.lock()?;
        for entity in entities {
            claims.remove(entity);
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

        // Once released, the entity is claimable again from epoch 0 —
        // mirroring a fresh Postgres `INSERT` after the row is deleted.
        store.release(&e, &me(), first).await.expect("release");
        let reacquired = store
            .acquire(&e, &me())
            .await
            .expect("re-acquire after release");
        assert_eq!(reacquired, ClaimEpoch(0));
    }

    #[tokio::test]
    async fn release_is_epoch_gated_and_idempotent() {
        // Mirrors `PostgresClaimStore`'s `release_is_epoch_gated_and_idempotent`
        // (`waddle-server/src/clustering/claims.rs`): a losing epoch is a
        // no-op — the claim survives — and releasing an absent claim is
        // not an error.
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
            .expect("stale release is a no-op, not an error");
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
    async fn release_many_clears_every_listed_entity() {
        let store = InProcessClaimStore::new();
        let a = entity("stream-a");
        let b = entity("stream-b");
        store.acquire(&a, &me()).await.expect("acquire a");
        store.acquire(&b, &me()).await.expect("acquire b");

        store
            .release_many(&[a.clone(), b.clone()], &me())
            .await
            .expect("release_many");

        assert!(!store
            .fence(&a, &me(), ClaimEpoch(0))
            .await
            .expect("fence a"));
        assert!(!store
            .fence(&b, &me(), ClaimEpoch(0))
            .await
            .expect("fence b"));
    }
}
