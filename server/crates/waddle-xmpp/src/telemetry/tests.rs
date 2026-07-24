//! Telemetry-foundation tests: every assertion reads exported
//! samples through the in-memory reader seam, never internal state.

use super::attributes::{
    Janitor, MessageKind, MetricAttribute, SessionInitFailureReason, StanzaErrorCondition,
    SweepOutcome,
};
use super::test_support;
use super::validate_metric_name;
use std::time::Duration;

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
}
