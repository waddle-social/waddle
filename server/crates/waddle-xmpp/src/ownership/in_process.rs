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

/// In-memory claim bookkeeping for the single-node case.
#[derive(Default)]
pub struct InProcessClaimStore {
    claims: Mutex<HashMap<Entity, ClaimEpoch>>,
}

impl InProcessClaimStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<Entity, ClaimEpoch>>, ClaimError> {
        self.claims.lock().map_err(|_| ClaimError::Poisoned)
    }
}

#[async_trait]
impl ClaimStore for InProcessClaimStore {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        // No backing schema — purely in-memory.
        Ok(())
    }

    async fn acquire(&self, entity: &Entity, _me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
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
        claims.insert(entity.clone(), ClaimEpoch(0));
        Ok(ClaimEpoch(0))
    }

    async fn steal_stale(
        &self,
        entity: &Entity,
        _observed: ClaimEpoch,
        _staleness: StalePredicate,
        _me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        // Single node: there is no other node to steal from, so this
        // trivially succeeds by bumping the epoch (mirroring what a fresh
        // acquire-after-steal would observe on the real CAS).
        let mut claims = self.lock()?;
        let entry = claims.entry(entity.clone()).or_insert(ClaimEpoch(0));
        entry.0 += 1;
        Ok(*entry)
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
        Ok(claims.get(entity) == Some(&mine))
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
        if claims.get(entity) == Some(&mine) {
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
