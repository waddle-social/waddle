//! Typed OpenTelemetry counters for push-pipeline transitions.

use super::attributes::{PushProvider, PushStage};
use crate::push::types::{TransientFailure, WebPushOutcome};

fn increment_pipeline(stage: PushStage) {
    crate::counter_add!(
        "waddle.push.pipeline",
        "{notification}",
        "Push notification pipeline transitions.",
        1,
        stage,
    );
}

/// Record a candidate that entered the durable notification pipeline.
pub fn increment_candidate_created() {
    increment_pipeline(PushStage::CandidateCreated);
}

/// Record a duplicate candidate coalesced at durable insertion.
pub fn increment_coalesced() {
    increment_pipeline(PushStage::Coalesced);
}

/// Record an XEP-0357 outbox job accepted by the Push Service.
pub fn increment_published() {
    increment_pipeline(PushStage::Published);
}

fn increment_provider(stage: PushStage, provider: PushProvider) {
    crate::counter_add!(
        "waddle.push.provider",
        "{notification}",
        "Push notification outcomes at the downstream provider boundary.",
        1,
        stage,
        provider,
    );
}

/// Record a notification accepted by a downstream provider.
pub fn increment_provider_sent(provider: PushProvider) {
    increment_provider(PushStage::ProviderSent, provider);
}

/// Record a notification rejected by a downstream provider.
pub fn increment_provider_rejected(provider: PushProvider) {
    increment_provider(PushStage::ProviderRejected, provider);
}

/// Record a downstream provider's expired-token response.
pub fn increment_provider_token_expired(provider: PushProvider) {
    increment_provider(PushStage::ProviderTokenExpired, provider);
}

/// Classify and record a typed result returned by the live Web Push provider.
///
/// Network and timeout failures do not increment the provider family because
/// no provider response was received. Provider-side 5xx responses do: the
/// provider actively rejected that attempt, even though the job remains
/// retryable.
pub fn record_web_push_outcome(outcome: &WebPushOutcome) -> Option<PushStage> {
    let stage = match outcome {
        WebPushOutcome::Delivered { .. } => PushStage::ProviderSent,
        WebPushOutcome::SubscriptionGone { .. } => PushStage::ProviderTokenExpired,
        WebPushOutcome::ClockSkew { .. }
        | WebPushOutcome::RateLimited { .. }
        | WebPushOutcome::PayloadTooLarge { .. }
        | WebPushOutcome::BadRequest { status: 1.. }
        | WebPushOutcome::Transient {
            kind: TransientFailure::ServerError { .. },
        } => PushStage::ProviderRejected,
        WebPushOutcome::BadRequest { status: 0 }
        | WebPushOutcome::Transient {
            kind: TransientFailure::Network | TransientFailure::Timeout,
        } => return None,
    };
    match stage {
        PushStage::ProviderSent => increment_provider_sent(PushProvider::WebPush),
        PushStage::ProviderRejected => increment_provider_rejected(PushProvider::WebPush),
        PushStage::ProviderTokenExpired => {
            increment_provider_token_expired(PushProvider::WebPush);
        }
        PushStage::CandidateCreated
        | PushStage::Suppressed
        | PushStage::Coalesced
        | PushStage::Published
        | PushStage::RetryScheduled
        | PushStage::DeadLettered => unreachable!("provider classifier returned pipeline stage"),
    }
    Some(stage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn candidate_created_exports_through_the_metric_reader() {
        let guard = crate::telemetry::test_support::acquire().await;
        increment_candidate_created();
        assert_eq!(
            guard.counter_sum("waddle.push.pipeline", &[("stage", "candidate_created")]),
            Some(1)
        );
    }

    #[tokio::test]
    async fn coalesced_exports_through_the_metric_reader() {
        let guard = crate::telemetry::test_support::acquire().await;
        increment_coalesced();
        assert_eq!(
            guard.counter_sum("waddle.push.pipeline", &[("stage", "coalesced")]),
            Some(1)
        );
    }

    #[tokio::test]
    async fn published_exports_with_the_notification_unit() {
        let guard = crate::telemetry::test_support::acquire().await;
        increment_published();
        assert_eq!(
            guard.counter_sum("waddle.push.pipeline", &[("stage", "published")]),
            Some(1)
        );
        assert_eq!(
            guard.metric_unit("waddle.push.pipeline"),
            Some("{notification}".to_string())
        );
    }

    #[tokio::test]
    async fn delivered_provider_outcome_exports_sent() {
        let guard = crate::telemetry::test_support::acquire().await;
        record_web_push_outcome(&WebPushOutcome::Delivered { status: 201 });
        assert_eq!(
            guard.counter_sum(
                "waddle.push.provider",
                &[("stage", "provider_sent"), ("provider", "web_push")]
            ),
            Some(1)
        );
    }

    #[tokio::test]
    async fn rejected_provider_outcome_exports_rejected() {
        let guard = crate::telemetry::test_support::acquire().await;
        record_web_push_outcome(&WebPushOutcome::BadRequest { status: 400 });
        assert_eq!(
            guard.counter_sum(
                "waddle.push.provider",
                &[("stage", "provider_rejected"), ("provider", "web_push")]
            ),
            Some(1)
        );
    }

    #[tokio::test]
    async fn local_preflight_failure_does_not_export_provider_rejection() {
        let guard = crate::telemetry::test_support::acquire().await;
        assert_eq!(
            record_web_push_outcome(&WebPushOutcome::BadRequest { status: 0 }),
            None
        );
        assert_eq!(guard.counter_sum("waddle.push.provider", &[]), None);
    }

    #[tokio::test]
    async fn gone_provider_outcome_exports_token_expired_with_notification_unit() {
        let guard = crate::telemetry::test_support::acquire().await;
        record_web_push_outcome(&WebPushOutcome::SubscriptionGone { status: 410 });
        assert_eq!(
            guard.counter_sum(
                "waddle.push.provider",
                &[
                    ("stage", "provider_token_expired"),
                    ("provider", "web_push")
                ]
            ),
            Some(1)
        );
        assert_eq!(
            guard.metric_unit("waddle.push.provider"),
            Some("{notification}".to_string())
        );
    }
}
