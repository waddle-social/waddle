//! Closed, cardinality-bounded observations for external [`ClaimStore`] calls.
//!
//! The decorator records one terminal observation around each external logical
//! mutation or advisory fence call. Implementations remain free to compose
//! methods internally without producing duplicate samples.

use async_trait::async_trait;
use std::sync::Arc;

use super::{
    ClaimEpoch, ClaimError, ClaimSnapshot, ClaimStore, Entity, ExactReleaseOutcome, NodeIdentity,
    ResumeIdentityProof, StalePredicate,
};
use crate::telemetry::attributes::{ClaimOp, ClaimResult, FenceResult, MetricAttribute};

/// Closed rejection detail retained on tracing observations, never as a metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimRejection {
    AlreadyClaimed,
    Conflict,
    Draining,
    AuthorityDisabled,
    Poisoned,
    ExcludedFromStealIntent,
    NotOwned,
}

impl ClaimRejection {
    const fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyClaimed => "already_claimed",
            Self::Conflict => "conflict",
            Self::Draining => "draining",
            Self::AuthorityDisabled => "authority_disabled",
            Self::Poisoned => "poisoned",
            Self::ExcludedFromStealIntent => "excluded_from_steal_intent",
            Self::NotOwned => "not_owned",
        }
    }
}

impl std::fmt::Display for ClaimRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Emits a `cancelled` fence sample if the caller's deadline drops the
/// in-flight fence future before the store answers (room publication and SM
/// hydration both bound this call with `tokio::time::timeout`). Without the
/// guard, exactly the outage samples this family exists to expose would be
/// the ones never recorded.
struct FenceCancellationGuard {
    armed: bool,
}

impl FenceCancellationGuard {
    fn new() -> Self {
        Self { armed: true }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for FenceCancellationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        crate::counter_add!(
            "waddle.clustering.fence.results",
            "{check}",
            "Advisory ownership fence checks by bounded result.",
            1,
            FenceResult::Cancelled,
        );
        tracing::debug!(
            result = %FenceResult::Cancelled.value(),
            "ownership fence cancelled at the caller's deadline"
        );
    }
}

/// Mutation counterpart of [`FenceCancellationGuard`]. A dropped mutation
/// future records into a dedicated one-dimension cancellation counter
/// instead of a `claim.mutations` result: `operation x result` already sits
/// at the documented series ceiling, and the decorator cannot distinguish a
/// caller deadline (storage stalled) from a lifecycle cancellation token
/// (healthy shutdown/worker restart racing an in-flight call) — classifying
/// either as `backend` would pollute the ownership-health signal, so the
/// counter stays provenance-neutral.
struct MutationCancellationGuard {
    operation: ClaimOp,
    armed: bool,
}

impl MutationCancellationGuard {
    fn new(operation: ClaimOp) -> Self {
        Self {
            operation,
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for MutationCancellationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        crate::counter_add!(
            "waddle.clustering.claim.cancellations",
            "{mutation}",
            "Ownership claim mutations whose in-flight future was dropped \
             (caller deadline or lifecycle cancellation) before the store answered.",
            1,
            self.operation,
        );
        tracing::debug!(
            operation = %self.operation.value(),
            "ownership claim mutation future dropped before the store answered"
        );
    }
}

/// Observes a concrete claim store without changing its ownership semantics.
pub struct ObservedClaimStore<S> {
    inner: S,
}

impl<S> ObservedClaimStore<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

pub fn observed_claim_store<S>(inner: S) -> Arc<dyn ClaimStore>
where
    S: ClaimStore + 'static,
{
    Arc::new(ObservedClaimStore::new(inner))
}

fn classify_error(error: &ClaimError) -> (ClaimResult, Option<ClaimRejection>) {
    match error {
        ClaimError::Backend(_) => (ClaimResult::Backend, None),
        ClaimError::AlreadyClaimed => (ClaimResult::Rejected, Some(ClaimRejection::AlreadyClaimed)),
        ClaimError::Conflict => (ClaimResult::Rejected, Some(ClaimRejection::Conflict)),
        ClaimError::Poisoned => (ClaimResult::Rejected, Some(ClaimRejection::Poisoned)),
        ClaimError::SmSessionExcludedFromStealIntent => (
            ClaimResult::Rejected,
            Some(ClaimRejection::ExcludedFromStealIntent),
        ),
        ClaimError::Draining => (ClaimResult::Rejected, Some(ClaimRejection::Draining)),
        ClaimError::AuthorityDisabled => (
            ClaimResult::Rejected,
            Some(ClaimRejection::AuthorityDisabled),
        ),
    }
}

fn observe_mutation(
    operation: ClaimOp,
    entity: Option<&Entity>,
    epoch: Option<ClaimEpoch>,
    result: ClaimResult,
    rejection: Option<ClaimRejection>,
) {
    crate::counter_add!(
        "waddle.clustering.claim.mutations",
        "{mutation}",
        "External ownership claim mutation attempts by operation and bounded result.",
        1,
        operation,
        result,
    );
    tracing::debug!(
        operation = %operation.value(),
        result = %result.value(),
        rejection = rejection.map(ClaimRejection::as_str),
        entity_kind = ?entity.map(|value| value.entity_type),
        epoch = epoch.map(|value| value.0),
        "ownership claim mutation observed"
    );
}

fn observe_result<T>(
    operation: ClaimOp,
    entity: Option<&Entity>,
    epoch: Option<ClaimEpoch>,
    result: &Result<T, ClaimError>,
) {
    let (class, rejection) = match result {
        Ok(_) => (ClaimResult::Ok, None),
        Err(error) => classify_error(error),
    };
    observe_mutation(operation, entity, epoch, class, rejection);
}

// `release()`/`release_many()` cannot observe a not-owned outcome by API
// shape (`Result<(), _>` — no affected-row signal); only `release_exact`
// distinguishes `NotOwned`. Their errors still classify through the shared
// mapping so a typed rejection is never mislabeled as a backend failure.
fn observe_release_result<T>(
    operation: ClaimOp,
    entity: Option<&Entity>,
    epoch: Option<ClaimEpoch>,
    result: &Result<T, ClaimError>,
) {
    observe_result(operation, entity, epoch, result);
}

#[async_trait]
impl<S: ClaimStore> ClaimStore for ObservedClaimStore<S> {
    async fn ensure_schema(&self) -> Result<(), ClaimError> {
        self.inner.ensure_schema().await
    }

    async fn acquire(&self, entity: &Entity, me: &NodeIdentity) -> Result<ClaimEpoch, ClaimError> {
        let guard = MutationCancellationGuard::new(ClaimOp::Acquire);
        let result = self.inner.acquire(entity, me).await;
        guard.disarm();
        observe_result(ClaimOp::Acquire, Some(entity), None, &result);
        result
    }

    async fn ensure_claimed(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        let guard = MutationCancellationGuard::new(ClaimOp::EnsureClaimed);
        let result = self.inner.ensure_claimed(entity, me).await;
        guard.disarm();
        observe_result(ClaimOp::EnsureClaimed, Some(entity), None, &result);
        result
    }

    async fn steal_stale(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        staleness: StalePredicate,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        let guard = MutationCancellationGuard::new(ClaimOp::StealStale);
        let result = self
            .inner
            .steal_stale(entity, observed, staleness, me)
            .await;
        guard.disarm();
        observe_result(ClaimOp::StealStale, Some(entity), Some(observed), &result);
        result
    }

    async fn steal_for_resume(
        &self,
        entity: &Entity,
        observed: ClaimEpoch,
        witness: ResumeIdentityProof,
        me: &NodeIdentity,
    ) -> Result<ClaimEpoch, ClaimError> {
        let guard = MutationCancellationGuard::new(ClaimOp::StealForResume);
        let result = self
            .inner
            .steal_for_resume(entity, observed, witness, me)
            .await;
        guard.disarm();
        observe_result(
            ClaimOp::StealForResume,
            Some(entity),
            Some(observed),
            &result,
        );
        result
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
        let guard = FenceCancellationGuard::new();
        let result = self.inner.fence(entity, me, mine).await;
        guard.disarm();
        let (class, rejection) = match &result {
            Ok(true) => (FenceResult::Ok, None),
            Ok(false) => (FenceResult::Rejected, Some(ClaimRejection::NotOwned)),
            Err(ClaimError::Backend(_)) => (FenceResult::Backend, None),
            Err(error) => (FenceResult::Rejected, classify_error(error).1),
        };
        crate::counter_add!(
            "waddle.clustering.fence.results",
            "{check}",
            "Advisory ownership fence checks by bounded result.",
            1,
            class,
        );
        tracing::debug!(
            result = %class.value(),
            rejection = rejection.map(ClaimRejection::as_str),
            entity_kind = ?entity.entity_type,
            epoch = mine.0,
            "ownership fence observed"
        );
        result
    }

    async fn release(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<(), ClaimError> {
        let guard = MutationCancellationGuard::new(ClaimOp::Release);
        let result = self.inner.release(entity, me, mine).await;
        guard.disarm();
        observe_release_result(ClaimOp::Release, Some(entity), Some(mine), &result);
        result
    }

    async fn release_exact(
        &self,
        entity: &Entity,
        me: &NodeIdentity,
        mine: ClaimEpoch,
    ) -> Result<ExactReleaseOutcome, ClaimError> {
        let guard = MutationCancellationGuard::new(ClaimOp::ReleaseExact);
        let result = self.inner.release_exact(entity, me, mine).await;
        guard.disarm();
        match &result {
            Ok(ExactReleaseOutcome::Released) => observe_mutation(
                ClaimOp::ReleaseExact,
                Some(entity),
                Some(mine),
                ClaimResult::Ok,
                None,
            ),
            Ok(ExactReleaseOutcome::NotOwned) => observe_mutation(
                ClaimOp::ReleaseExact,
                Some(entity),
                Some(mine),
                ClaimResult::Rejected,
                Some(ClaimRejection::NotOwned),
            ),
            Err(error) => {
                let (class, rejection) = classify_error(error);
                observe_mutation(
                    ClaimOp::ReleaseExact,
                    Some(entity),
                    Some(mine),
                    class,
                    rejection,
                );
            }
        }
        result
    }

    async fn release_many(&self, entities: &[Entity], me: &NodeIdentity) -> Result<(), ClaimError> {
        let guard = MutationCancellationGuard::new(ClaimOp::ReleaseMany);
        let result = self.inner.release_many(entities, me).await;
        guard.disarm();
        observe_release_result(ClaimOp::ReleaseMany, None, None, &result);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_errors_map_to_closed_result_classes() {
        assert_eq!(
            classify_error(&ClaimError::Backend("secret".into())).0,
            ClaimResult::Backend
        );
        for error in [
            ClaimError::AlreadyClaimed,
            ClaimError::Conflict,
            ClaimError::Poisoned,
            ClaimError::SmSessionExcludedFromStealIntent,
            ClaimError::Draining,
            ClaimError::AuthorityDisabled,
        ] {
            let (result, detail) = classify_error(&error);
            assert_eq!(result, ClaimResult::Rejected);
            assert!(detail.is_some());
        }
    }

    #[tokio::test]
    async fn cancelled_calls_emit_bounded_samples() {
        use super::super::EntityType;

        struct PendingStore;

        #[async_trait]
        impl ClaimStore for PendingStore {
            async fn ensure_schema(&self) -> Result<(), ClaimError> {
                std::future::pending().await
            }
            async fn acquire(
                &self,
                _entity: &Entity,
                _me: &NodeIdentity,
            ) -> Result<ClaimEpoch, ClaimError> {
                std::future::pending().await
            }
            async fn ensure_claimed(
                &self,
                _entity: &Entity,
                _me: &NodeIdentity,
            ) -> Result<ClaimEpoch, ClaimError> {
                std::future::pending().await
            }
            async fn steal_stale(
                &self,
                _entity: &Entity,
                _observed: ClaimEpoch,
                _staleness: StalePredicate,
                _me: &NodeIdentity,
            ) -> Result<ClaimEpoch, ClaimError> {
                std::future::pending().await
            }
            async fn steal_for_resume(
                &self,
                _entity: &Entity,
                _observed: ClaimEpoch,
                _witness: ResumeIdentityProof,
                _me: &NodeIdentity,
            ) -> Result<ClaimEpoch, ClaimError> {
                std::future::pending().await
            }
            async fn current_claim(
                &self,
                _entity: &Entity,
            ) -> Result<Option<ClaimSnapshot>, ClaimError> {
                std::future::pending().await
            }
            async fn current_claim_after_pending_writes(
                &self,
                _entity: &Entity,
            ) -> Result<Option<ClaimSnapshot>, ClaimError> {
                std::future::pending().await
            }
            async fn fence(
                &self,
                _entity: &Entity,
                _me: &NodeIdentity,
                _mine: ClaimEpoch,
            ) -> Result<bool, ClaimError> {
                std::future::pending().await
            }
            async fn release(
                &self,
                _entity: &Entity,
                _me: &NodeIdentity,
                _mine: ClaimEpoch,
            ) -> Result<(), ClaimError> {
                std::future::pending().await
            }
            async fn release_exact(
                &self,
                _entity: &Entity,
                _me: &NodeIdentity,
                _mine: ClaimEpoch,
            ) -> Result<ExactReleaseOutcome, ClaimError> {
                std::future::pending().await
            }
            async fn release_many(
                &self,
                _entities: &[Entity],
                _me: &NodeIdentity,
            ) -> Result<(), ClaimError> {
                std::future::pending().await
            }
        }

        let metrics = crate::telemetry::test_support::acquire().await;
        let store = ObservedClaimStore::new(PendingStore);
        let entity = Entity::new(EntityType::SmSession, "cancelled-observation");
        let me = NodeIdentity::new("node", "incarnation");

        let fence = tokio::time::timeout(
            std::time::Duration::from_millis(10),
            store.fence(&entity, &me, ClaimEpoch(1)),
        )
        .await;
        assert!(fence.is_err(), "pending fence must hit the deadline");
        assert_eq!(
            metrics.counter_sum(
                "waddle.clustering.fence.results",
                &[("result", "cancelled")]
            ),
            Some(1),
            "a deadline-dropped fence must record a cancelled sample"
        );

        let acquire = tokio::time::timeout(
            std::time::Duration::from_millis(10),
            store.acquire(&entity, &me),
        )
        .await;
        assert!(acquire.is_err(), "pending acquire must hit the deadline");
        assert_eq!(
            metrics.counter_sum(
                "waddle.clustering.claim.cancellations",
                &[("operation", "acquire")]
            ),
            Some(1),
            "a dropped mutation future must record a cancellation sample"
        );
        assert_eq!(
            metrics
                .counter_sum(
                    "waddle.clustering.claim.mutations",
                    &[("operation", "acquire"), ("result", "backend")]
                )
                .unwrap_or(0),
            0,
            "a dropped future must never masquerade as a backend failure"
        );
    }
}
