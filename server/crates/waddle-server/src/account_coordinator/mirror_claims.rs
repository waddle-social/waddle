//! The claim store the `UserActor` mirror uses while the coordinator is the
//! account authority.

use std::sync::Arc;

use async_trait::async_trait;
use jid::BareJid;
use waddle_xmpp::ownership::{
    ClaimEpoch, ClaimError, ClaimSnapshot, ClaimStore, Entity, EntityType, ExactReleaseOutcome,
    NodeIdentity, ResumeIdentityProof, StalePredicate,
};

use super::directory::AccountCoordinators;

/// Finds the coordinators of this node, once they are the selected account
/// authority. `None` while the connection registry is the authority.
pub type CoordinatorLookup = Arc<dyn Fn() -> Option<Arc<AccountCoordinators>> + Send + Sync>;

/// A claim store for the user registry that shares each account's
/// `UserActor` claim with the account's coordinator.
///
/// The user registry takes an account's claim when it registers the
/// account's first resource and releases it when the last one is gone. The
/// coordinator needs the same claim for as long as the account holds a
/// binding, which includes a detached binding that has no registry entry.
/// This store records which claims the registry holds, and turns the
/// registry's release into a request to the coordinator: the claim row is
/// deleted only once the account holds no binding either. Without that, a
/// transport loss that leaves a resumable session would release the claim
/// under the detached binding, and every later write for it would be
/// refused.
///
/// Claims of other entity types, and every call made while the connection
/// registry is the account authority, go to the inner store unchanged.
pub struct MirrorClaimStore {
    inner: Arc<dyn ClaimStore>,
    coordinators: CoordinatorLookup,
}

impl MirrorClaimStore {
    pub fn new(inner: Arc<dyn ClaimStore>, coordinators: CoordinatorLookup) -> Self {
        Self {
            inner,
            coordinators,
        }
    }

    /// The account and its coordinators when `entity` is an account claim
    /// and the coordinator is the account authority.
    fn account(&self, entity: &Entity) -> Option<(BareJid, Arc<AccountCoordinators>)> {
        if entity.entity_type != EntityType::UserActor {
            return None;
        }
        let account = entity.id.parse::<BareJid>().ok()?;
        Some((account, (self.coordinators)()?))
    }

    /// Run an acquisition and record the hold it produced. The hold lock is
    /// taken first, so the coordinator cannot release the claim between the
    /// acquisition and its record.
    async fn acquire_recorded<F>(
        &self,
        entity: &Entity,
        acquire: F,
    ) -> Result<ClaimEpoch, ClaimError>
    where
        F: std::future::Future<Output = Result<ClaimEpoch, ClaimError>> + Send,
    {
        let Some((account, coordinators)) = self.account(entity) else {
            return acquire.await;
        };
        let mut held = coordinators.mirror_holds().held.lock().await;
        let epoch = acquire.await?;
        held.insert(account, epoch);
        Ok(epoch)
    }
}

#[async_trait]
impl ClaimStore for MirrorClaimStore {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        self.inner.ensure_schema().await
    }

    async fn acquire(&self, entity: &Entity, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        self.acquire_recorded(entity, self.inner.acquire(entity, me))
            .await
    }

    async fn ensure_claimed(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        self.acquire_recorded(entity, self.inner.ensure_claimed(entity, me))
            .await
    }

    async fn steal_stale(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        staleness: StalePredicate,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        self.acquire_recorded(
            entity,
            self.inner.steal_stale(entity, observed, staleness, me),
        )
        .await
    }

    async fn steal_for_resume(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        witness: ResumeIdentityProof,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        self.inner
            .steal_for_resume(entity, observed, witness, me)
            .await
    }

    async fn current_claim(&self, entity: &Entity) -> Result<Option<ClaimSnapshot>, ClaimError> {
        self.inner.current_claim(entity).await
    }

    async fn current_claim_after_pending_writes(
        &self,
        entity: &Entity,
    ) -> Result<Option<ClaimSnapshot>, ClaimError> {
        self.inner.current_claim_after_pending_writes(entity).await
    }

    async fn fence(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<bool, ClaimError> {
        self.inner.fence(entity, me, mine).await
    }

    /// Drop the registry's hold on an account claim and let the account's
    /// coordinator release the claim if the account holds no binding. A
    /// release that names an epoch other than the recorded hold is an old
    /// lease of the registry: it goes to the inner store, which deletes
    /// only that exact row.
    async fn release(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<(), ClaimError> {
        let Some((account, coordinators)) = self.account(entity) else {
            return self.inner.release(entity, me, mine).await;
        };
        let recorded = {
            let mut held = coordinators.mirror_holds().held.lock().await;
            let recorded = held.get(&account).copied();
            if recorded == Some(mine) {
                held.remove(&account);
            }
            recorded
        };
        if recorded.is_some_and(|epoch| epoch != mine) {
            return self.inner.release(entity, me, mine).await;
        }
        coordinators
            .release_claim_if_unbound(&account)
            .await
            .map(|_| ())
            .map_err(|error| ClaimError::Backend(error.to_string()))
    }

    async fn release_exact(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<ExactReleaseOutcome, ClaimError> {
        self.inner.release_exact(entity, me, mine).await
    }

    async fn release_many(&self, entities: &[Entity], me: &NodeIdentity) -> Result<(), ClaimError> {
        self.inner.release_many(entities, me).await
    }

    async fn release_many_exact(
        &self,
        claims: &[(Entity, ClaimEpoch)],
        me: &NodeIdentity,
    ) -> Result<(), ClaimError> {
        self.inner.release_many_exact(claims, me).await
    }
}
