//! Telemetry-foundation tests: every assertion reads exported
//! samples through the in-memory reader seam, never internal state.

use super::attributes::{
    CallControlRateLimitedSurface, Janitor, MessageKind, MetricAttribute, SessionInitFailureReason,
    StanzaErrorCondition, SweepOutcome,
};
use super::test_support;
use super::validate_metric_name;
use async_trait::async_trait;
use std::time::Duration;

#[tokio::test]
async fn observed_claim_store_exports_one_sample_per_external_operation() {
    use crate::ownership::{
        verify_resume_identity, ClaimStore, Entity, EntityType, ExactReleaseOutcome,
        InProcessClaimStore, NodeIdentity, ObservedClaimStore, StalePredicate,
    };

    let guard = test_support::acquire().await;
    let store = ObservedClaimStore::new(InProcessClaimStore::new());
    let owner = NodeIdentity::new("owner", "epoch-a");
    let successor = NodeIdentity::new("successor", "epoch-b");
    let entity = |id| Entity::new(EntityType::SmSession, id);

    let acquired = store
        .acquire(&entity("acquire"), &owner)
        .await
        .expect("acquire");
    assert!(store.acquire(&entity("acquire"), &successor).await.is_err());
    let ensured = store
        .ensure_claimed(&entity("ensure"), &owner)
        .await
        .expect("ensure");
    assert!(store
        .fence(&entity("ensure"), &owner, ensured)
        .await
        .expect("fence"));
    assert!(!store
        .fence(&entity("ensure"), &successor, ensured)
        .await
        .expect("rejected fence"));
    let stolen = store
        .steal_stale(
            &entity("acquire"),
            acquired,
            StalePredicate::OwnerStale,
            &successor,
        )
        .await
        .expect("steal stale");
    let jid = "alice@example.com".parse().expect("jid");
    let proof = verify_resume_identity(&jid, &jid).expect("proof");
    let resumed = store
        .steal_for_resume(&entity("acquire"), stolen, proof, &owner)
        .await
        .expect("steal for resume");
    store
        .release(&entity("missing"), &owner, resumed)
        .await
        .expect("release");
    assert_eq!(
        store
            .release_exact(&entity("missing"), &owner, resumed)
            .await
            .expect("release exact"),
        ExactReleaseOutcome::NotOwned
    );
    store.release_many(&[], &owner).await.expect("release many");

    for (operation, result) in [
        ("acquire", "ok"),
        ("acquire", "rejected"),
        ("ensure_claimed", "ok"),
        ("steal_stale", "ok"),
        ("steal_for_resume", "ok"),
        ("release", "ok"),
        ("release_exact", "rejected"),
        ("release_many", "ok"),
    ] {
        assert_eq!(
            guard.counter_sum(
                "waddle.clustering.claim.mutations",
                &[("operation", operation), ("result", result)],
            ),
            Some(1),
            "unexpected sample count for {operation}/{result}"
        );
    }
    assert_eq!(
        guard.counter_sum("waddle.clustering.fence.results", &[("result", "ok")]),
        Some(1)
    );
    assert_eq!(
        guard.counter_sum("waddle.clustering.fence.results", &[("result", "rejected")],),
        Some(1)
    );
}

struct AcquireDelegatingClaimStore {
    inner: crate::ownership::InProcessClaimStore,
}

#[async_trait]
impl crate::ownership::ClaimStore for AcquireDelegatingClaimStore {
    async fn ensure_schema(&self) -> Result<(), crate::ownership::ClaimError> {
        self.inner.ensure_schema().await
    }

    async fn acquire(
        &self,
        entity: &crate::ownership::Entity,
        me: &crate::ownership::NodeIdentity,
    ) -> Result<crate::ownership::ClaimEpoch, crate::ownership::ClaimError> {
        self.inner.acquire(entity, me).await
    }

    async fn ensure_claimed(
        &self,
        entity: &crate::ownership::Entity,
        me: &crate::ownership::NodeIdentity,
    ) -> Result<crate::ownership::ClaimEpoch, crate::ownership::ClaimError> {
        self.inner.acquire(entity, me).await
    }

    async fn steal_stale(
        &self,
        entity: &crate::ownership::Entity,
        observed: crate::ownership::ClaimEpoch,
        staleness: crate::ownership::StalePredicate,
        me: &crate::ownership::NodeIdentity,
    ) -> Result<crate::ownership::ClaimEpoch, crate::ownership::ClaimError> {
        self.inner
            .steal_stale(entity, observed, staleness, me)
            .await
    }

    async fn steal_for_resume(
        &self,
        entity: &crate::ownership::Entity,
        observed: crate::ownership::ClaimEpoch,
        witness: crate::ownership::ResumeIdentityProof,
        me: &crate::ownership::NodeIdentity,
    ) -> Result<crate::ownership::ClaimEpoch, crate::ownership::ClaimError> {
        self.inner
            .steal_for_resume(entity, observed, witness, me)
            .await
    }

    async fn current_claim(
        &self,
        entity: &crate::ownership::Entity,
    ) -> Result<Option<crate::ownership::ClaimSnapshot>, crate::ownership::ClaimError> {
        self.inner.current_claim(entity).await
    }

    async fn current_claim_after_pending_writes(
        &self,
        entity: &crate::ownership::Entity,
    ) -> Result<Option<crate::ownership::ClaimSnapshot>, crate::ownership::ClaimError> {
        self.inner.current_claim_after_pending_writes(entity).await
    }

    async fn fence(
        &self,
        entity: &crate::ownership::Entity,
        me: &crate::ownership::NodeIdentity,
        mine: crate::ownership::ClaimEpoch,
    ) -> Result<bool, crate::ownership::ClaimError> {
        self.inner.fence(entity, me, mine).await
    }

    async fn release(
        &self,
        entity: &crate::ownership::Entity,
        me: &crate::ownership::NodeIdentity,
        mine: crate::ownership::ClaimEpoch,
    ) -> Result<(), crate::ownership::ClaimError> {
        self.inner.release(entity, me, mine).await
    }

    async fn release_exact(
        &self,
        entity: &crate::ownership::Entity,
        me: &crate::ownership::NodeIdentity,
        mine: crate::ownership::ClaimEpoch,
    ) -> Result<crate::ownership::ExactReleaseOutcome, crate::ownership::ClaimError> {
        self.inner.release_exact(entity, me, mine).await
    }

    async fn release_many(
        &self,
        entities: &[crate::ownership::Entity],
        me: &crate::ownership::NodeIdentity,
    ) -> Result<(), crate::ownership::ClaimError> {
        self.inner.release_many(entities, me).await
    }
}

#[tokio::test]
async fn observed_claim_store_counts_only_the_outer_ensure_claimed_call() {
    use crate::ownership::{ClaimStore, Entity, EntityType, NodeIdentity, ObservedClaimStore};

    let guard = test_support::acquire().await;
    let store = ObservedClaimStore::new(AcquireDelegatingClaimStore {
        inner: crate::ownership::InProcessClaimStore::new(),
    });
    let entity = Entity::new(EntityType::SmSession, "nested-ensure");
    let owner = NodeIdentity::new("owner", "epoch-a");

    store
        .ensure_claimed(&entity, &owner)
        .await
        .expect("ensure claimed");

    assert_eq!(
        guard.counter_sum(
            "waddle.clustering.claim.mutations",
            &[("operation", "ensure_claimed"), ("result", "ok")]
        ),
        Some(1)
    );
    assert_eq!(
        guard.counter_sum(
            "waddle.clustering.claim.mutations",
            &[("operation", "acquire"), ("result", "ok")]
        ),
        Some(0)
    );
}

#[tokio::test]
async fn bounded_flush_exports_shutdown_tail_counter() {
    let guard = test_support::acquire().await;

    super::reliability::increment_sm_drain_timeout();

    assert!(
        super::force_flush_bounded(&guard.provider(), Duration::from_secs(1)).await,
        "in-memory meter provider must flush within the bound"
    );
    assert_eq!(guard.counter_sum("xmpp.sm.drain_timeout", &[]), Some(1));
}

#[tokio::test]
async fn counter_is_created_at_first_increment_only() {
    let guard = test_support::acquire().await;

    assert!(
        !guard
            .metric_names()
            .contains(&"waddle.telemetry.selftest.lazy".to_string()),
        "instrument must not exist before the first increment"
    );
    assert_eq!(
        guard.counter_sum("waddle.telemetry.selftest.lazy", &[]),
        None
    );

    crate::counter_add!(
        "waddle.telemetry.selftest.lazy",
        "{event}",
        "Telemetry self-test counter: created at the increment site.",
        1,
    );

    assert_eq!(
        guard.counter_sum("waddle.telemetry.selftest.lazy", &[]),
        Some(1)
    );
}

#[tokio::test]
async fn counter_carries_ucum_unit() {
    let guard = test_support::acquire().await;

    crate::counter_add!(
        "waddle.telemetry.selftest.unit",
        "{message}",
        "Telemetry self-test counter: unit lands on the instrument.",
        1,
    );

    assert_eq!(
        guard.metric_unit("waddle.telemetry.selftest.unit"),
        Some("{message}".to_string())
    );
}

#[tokio::test]
async fn counter_attributes_render_enumerated_values() {
    let guard = test_support::acquire().await;

    crate::counter_add!(
        "waddle.telemetry.selftest.kinds",
        "{message}",
        "Telemetry self-test counter: enumerated kind attribute.",
        2,
        MessageKind::MucPm,
    );
    crate::counter_add!(
        "waddle.telemetry.selftest.kinds",
        "{message}",
        "Telemetry self-test counter: enumerated kind attribute.",
        3,
        MessageKind::Dm,
    );

    assert_eq!(
        guard.counter_sum("waddle.telemetry.selftest.kinds", &[("kind", "muc_pm")]),
        Some(2)
    );
    assert_eq!(
        guard.counter_sum("waddle.telemetry.selftest.kinds", &[("kind", "dm")]),
        Some(3)
    );
    assert_eq!(
        guard.counter_sum("waddle.telemetry.selftest.kinds", &[]),
        Some(5)
    );
}

#[tokio::test]
async fn counter_supports_multiple_attributes() {
    let guard = test_support::acquire().await;

    crate::counter_add!(
        "waddle.telemetry.selftest.sweeps",
        "{sweep}",
        "Telemetry self-test counter: janitor heartbeat shape.",
        1,
        Janitor::RoomDormancy,
        SweepOutcome::Completed,
    );

    assert_eq!(
        guard.counter_sum(
            "waddle.telemetry.selftest.sweeps",
            &[("janitor", "room_dormancy"), ("outcome", "completed")]
        ),
        Some(1)
    );
    assert_eq!(
        guard.counter_sum(
            "waddle.telemetry.selftest.sweeps",
            &[("janitor", "room_dormancy"), ("outcome", "failed")]
        ),
        Some(0)
    );
}

#[tokio::test]
async fn histogram_records_samples_with_attributes() {
    let guard = test_support::acquire().await;

    crate::histogram_record!(
        "waddle.telemetry.selftest.latency",
        "ms",
        "Telemetry self-test histogram.",
        12.5,
        StanzaErrorCondition::ServiceUnavailable,
    );
    crate::histogram_record!(
        "waddle.telemetry.selftest.latency",
        "ms",
        "Telemetry self-test histogram.",
        7.25,
        StanzaErrorCondition::ServiceUnavailable,
    );

    assert_eq!(
        guard.histogram_count(
            "waddle.telemetry.selftest.latency",
            &[("condition", "service-unavailable")]
        ),
        Some(2)
    );
    assert_eq!(
        guard.metric_unit("waddle.telemetry.selftest.latency"),
        Some("ms".to_string())
    );
}

#[tokio::test]
async fn histogram_buckets_form_pins_explicit_boundaries() {
    let guard = test_support::acquire().await;

    crate::histogram_record!(
        "waddle.telemetry.selftest.seconds",
        "s",
        "Telemetry self-test histogram: second-scale buckets.",
        buckets: super::SECOND_SCALE_BUCKETS,
        0.42,
    );

    assert_eq!(
        guard.histogram_bounds("waddle.telemetry.selftest.seconds"),
        Some(super::SECOND_SCALE_BUCKETS.to_vec()),
    );
}

#[tokio::test]
async fn consecutive_guards_observe_only_their_own_increments() {
    {
        let _first = test_support::acquire().await;
        crate::counter_add!(
            "waddle.telemetry.selftest.isolation",
            "{event}",
            "Telemetry self-test counter: guard isolation.",
            7,
        );
    }
    let second = test_support::acquire().await;
    // Delta temporality plus the acquire-time drain: increments made
    // under the first guard must be invisible to the second.
    assert_eq!(
        second.counter_sum("waddle.telemetry.selftest.isolation", &[]),
        None
    );
}

#[test]
fn valid_metric_names_pass_validation() {
    assert_eq!(
        validate_metric_name("waddle.sm.unacked.evicted"),
        "waddle.sm.unacked.evicted"
    );
    assert_eq!(
        validate_metric_name("waddle.janitor.sweeps"),
        "waddle.janitor.sweeps"
    );
    assert_eq!(
        validate_metric_name("waddle.push.outbox.retry_scheduled"),
        "waddle.push.outbox.retry_scheduled"
    );
    assert_eq!(
        validate_metric_name("waddle.http2.errors"),
        "waddle.http2.errors"
    );
}

#[test]
#[should_panic(expected = "dot.case")]
fn uppercase_metric_name_is_rejected() {
    let _ = validate_metric_name("waddle.SM.evicted");
}

#[test]
#[should_panic(expected = "empty segments")]
fn doubled_dot_is_rejected() {
    let _ = validate_metric_name("waddle..evicted");
}

#[test]
#[should_panic(expected = "must not end with '.'")]
fn trailing_dot_is_rejected() {
    let _ = validate_metric_name("waddle.evicted.");
}

#[test]
#[should_panic(expected = "start with a lowercase letter")]
fn segment_starting_with_digit_is_rejected() {
    let _ = validate_metric_name("waddle.2fast");
}

#[test]
#[should_panic(expected = "_total")]
fn prometheus_total_suffix_is_rejected() {
    let _ = validate_metric_name("waddle.messages_total");
}

#[tokio::test(flavor = "current_thread")]
async fn warn_events_leave_span_status_unset_error_events_mark_it() {
    // The production bridge maps ERROR-level events to span status
    // (#1428). Benign outcomes are logged at warn or below, so this
    // pins the contract that keeps `status=error` meaningful: warns
    // must never mark a span, errors must.
    let spans = test_support::acquire_spans();

    {
        let span = tracing::info_span!("benign_op");
        let _entered = span.enter();
        tracing::warn!("expected, benign outcome");
    }
    {
        let span = tracing::info_span!("failing_op");
        let _entered = span.enter();
        tracing::error!("operation failed");
    }

    assert_eq!(
        spans.status_of("benign_op"),
        Some(opentelemetry::trace::Status::Unset)
    );
    assert!(matches!(
        spans.status_of("failing_op"),
        Some(opentelemetry::trace::Status::Error { .. })
    ));
}

#[test]
fn attribute_enums_expose_stable_keys_and_values() {
    assert_eq!(MessageKind::Dm.key(), "kind");
    assert_eq!(MessageKind::MucPm.value(), "muc_pm");
    assert_eq!(Janitor::PendingDeliveryClaim.key(), "janitor");
    assert_eq!(
        Janitor::PendingDeliveryClaim.value(),
        "pending_delivery_claim"
    );
    assert_eq!(SweepOutcome::Failed.key(), "outcome");
    assert_eq!(SweepOutcome::Failed.value(), "failed");
    assert_eq!(StanzaErrorCondition::PolicyViolation.key(), "condition");
    assert_eq!(
        StanzaErrorCondition::PolicyViolation.value(),
        "policy-violation"
    );
    assert_eq!(SessionInitFailureReason::BlocklistLoad.key(), "reason");
    assert_eq!(
        SessionInitFailureReason::BlocklistLoad.value(),
        "blocklist_load"
    );
    assert_eq!(
        SessionInitFailureReason::AuthoritativeRegistration.value(),
        "authoritative_registration"
    );
    assert_eq!(CallControlRateLimitedSurface::Turn.key(), "surface");
    assert_eq!(
        CallControlRateLimitedSurface::MujiAction.value(),
        "muji_action"
    );
}
