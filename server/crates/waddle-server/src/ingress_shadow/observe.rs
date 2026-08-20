use waddle_xmpp::ingress::IngressOrdinal;
use waddle_xmpp::ownership::ClaimEpoch;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::telemetry::{
    attributes::{
        IngressAliasOutcome as MetricAliasOutcome, IngressDecisionClass as MetricDecisionClass,
        IngressSkipReason,
    },
    reliability,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressShadowDecisionClass {
    Accepted,
    ExistingSameDigest,
    IntentDivergence,
    RemoteRouteAmbiguous,
    AliasConflict,
    CaptureOverflow,
    SemanticMalformed,
    AuthorizationDenied,
    PrincipalMissing,
    ClaimFenceMissing,
    FrontierStale,
    Storage,
    SerializationExhaustion,
    SkippedUnenrolled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressShadowAliasOutcome {
    None,
    Inserted,
    Existing,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressShadowRequestKind {
    Enroll,
    Submit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressShadowDropReason {
    Disabled,
    QueueFull,
    ParkingFull,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressShadowCommitKind {
    Enrolled,
    Advanced,
    Idempotent,
    Stale,
    SkippedUnenrolled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressShadowObservation {
    Accepted {
        kind: IngressShadowRequestKind,
        stream_id: SmSessionId,
    },
    Dropped {
        kind: IngressShadowRequestKind,
        stream_id: SmSessionId,
        reason: IngressShadowDropReason,
    },
    Committed {
        stream_id: SmSessionId,
        claim_epoch: Option<ClaimEpoch>,
        handled_ordinal: Option<IngressOrdinal>,
        kind: IngressShadowCommitKind,
    },
    Failed {
        kind: IngressShadowRequestKind,
        stream_id: SmSessionId,
        claim_epoch: Option<ClaimEpoch>,
        handled_ordinal: Option<IngressOrdinal>,
    },
    Decision {
        stream_id: SmSessionId,
        claim_epoch: Option<ClaimEpoch>,
        handled_ordinal: Option<IngressOrdinal>,
        class: IngressShadowDecisionClass,
        alias: IngressShadowAliasOutcome,
    },
}

pub fn observe(observation: IngressShadowObservation) {
    match observation {
        IngressShadowObservation::Accepted { kind, stream_id } => {
            tracing::debug!(?kind, stream_id = %stream_id, "ingress shadow accepted");
            if matches!(kind, IngressShadowRequestKind::Submit) {
                reliability::increment_ingress_shadow_admissions();
            }
        }
        IngressShadowObservation::Dropped {
            kind,
            stream_id,
            reason,
        } => {
            tracing::debug!(
                ?kind,
                ?reason,
                stream_id = %stream_id,
                "ingress shadow dropped"
            );
            reliability::increment_ingress_shadow_skip(skip_reason(reason));
        }
        IngressShadowObservation::Committed {
            stream_id,
            claim_epoch,
            handled_ordinal,
            kind,
        } => {
            tracing::debug!(
                ?kind,
                stream_id = %stream_id,
                claim_epoch = claim_epoch.map(|epoch| epoch.0),
                handled_ordinal = handled_ordinal.map(|ordinal| ordinal.to_storage()),
                "ingress shadow committed"
            );
            if matches!(kind, IngressShadowCommitKind::SkippedUnenrolled) {
                reliability::increment_ingress_shadow_skip(IngressSkipReason::Unenrolled);
            }
        }
        IngressShadowObservation::Failed {
            kind,
            stream_id,
            claim_epoch,
            handled_ordinal,
        } => {
            tracing::warn!(
                ?kind,
                stream_id = %stream_id,
                claim_epoch = claim_epoch.map(|epoch| epoch.0),
                handled_ordinal = handled_ordinal.map(|ordinal| ordinal.to_storage()),
                "ingress shadow failed"
            );
            // Not a completion: every admitted submission terminates through
            // `Decision` (exhaustion and timeouts are typed decision classes)
            // or through the outstanding guard's `Aborted`; counting `Failed`
            // here too would double-count against that guard.
        }
        IngressShadowObservation::Decision {
            stream_id,
            claim_epoch,
            handled_ordinal,
            class,
            alias,
        } => {
            tracing::debug!(
                ?class,
                ?alias,
                stream_id = %stream_id,
                claim_epoch = claim_epoch.map(|epoch| epoch.0),
                handled_ordinal = handled_ordinal.map(|ordinal| ordinal.to_storage()),
                "ingress shadow decision"
            );
            if let Some(metric_class) = decision_class(class) {
                reliability::increment_ingress_shadow_decision(metric_class);
            }
            if let Some(metric_alias) = alias_outcome(alias) {
                reliability::increment_ingress_shadow_alias_outcome(metric_alias);
            }
            reliability::increment_ingress_shadow_completions();
        }
    }
}

fn skip_reason(reason: IngressShadowDropReason) -> IngressSkipReason {
    match reason {
        IngressShadowDropReason::Disabled => IngressSkipReason::Disabled,
        IngressShadowDropReason::QueueFull => IngressSkipReason::QueueFull,
        IngressShadowDropReason::ParkingFull => IngressSkipReason::ParkingFull,
        IngressShadowDropReason::Closed => IngressSkipReason::Closed,
    }
}

fn decision_class(class: IngressShadowDecisionClass) -> Option<MetricDecisionClass> {
    match class {
        IngressShadowDecisionClass::Accepted => Some(MetricDecisionClass::Accepted),
        IngressShadowDecisionClass::ExistingSameDigest => {
            Some(MetricDecisionClass::ExistingSameDigest)
        }
        IngressShadowDecisionClass::IntentDivergence => Some(MetricDecisionClass::IntentDivergence),
        IngressShadowDecisionClass::RemoteRouteAmbiguous => {
            Some(MetricDecisionClass::RemoteRouteAmbiguous)
        }
        IngressShadowDecisionClass::AliasConflict => Some(MetricDecisionClass::AliasConflict),
        IngressShadowDecisionClass::CaptureOverflow => Some(MetricDecisionClass::CaptureOverflow),
        IngressShadowDecisionClass::SemanticMalformed => {
            Some(MetricDecisionClass::SemanticMalformed)
        }
        IngressShadowDecisionClass::AuthorizationDenied => {
            Some(MetricDecisionClass::AuthorizationDenied)
        }
        IngressShadowDecisionClass::PrincipalMissing => Some(MetricDecisionClass::PrincipalMissing),
        IngressShadowDecisionClass::ClaimFenceMissing => {
            Some(MetricDecisionClass::ClaimFenceMissing)
        }
        IngressShadowDecisionClass::FrontierStale => Some(MetricDecisionClass::FrontierStale),
        IngressShadowDecisionClass::Storage => Some(MetricDecisionClass::Storage),
        IngressShadowDecisionClass::SerializationExhaustion => {
            Some(MetricDecisionClass::SerializationExhaustion)
        }
        IngressShadowDecisionClass::SkippedUnenrolled => None,
    }
}

fn alias_outcome(alias: IngressShadowAliasOutcome) -> Option<MetricAliasOutcome> {
    match alias {
        IngressShadowAliasOutcome::None => None,
        IngressShadowAliasOutcome::Inserted => Some(MetricAliasOutcome::Inserted),
        IngressShadowAliasOutcome::Existing => Some(MetricAliasOutcome::Existing),
        IngressShadowAliasOutcome::Conflict => Some(MetricAliasOutcome::Conflict),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream_id() -> SmSessionId {
        SmSessionId::new("telemetry-stream")
    }

    #[tokio::test]
    async fn submission_failed_observation_is_not_a_completion() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let completions_before = metrics
            .counter_sum("ingress.shadow.completions", &[])
            .unwrap_or(0);
        observe(IngressShadowObservation::Failed {
            kind: IngressShadowRequestKind::Submit,
            stream_id: stream_id(),
            claim_epoch: None,
            handled_ordinal: None,
        });
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.completions", &[])
                .unwrap_or(0),
            completions_before,
            "completions are decisions + aborted only"
        );
    }

    #[tokio::test]
    async fn submission_admission_and_terminal_observations_balance() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let admissions_before = metrics
            .counter_sum("ingress.shadow.admissions", &[])
            .unwrap_or(0);
        let completions_before = metrics
            .counter_sum("ingress.shadow.completions", &[])
            .unwrap_or(0);
        observe(IngressShadowObservation::Accepted {
            kind: IngressShadowRequestKind::Submit,
            stream_id: stream_id(),
        });
        observe(IngressShadowObservation::Decision {
            stream_id: stream_id(),
            claim_epoch: None,
            handled_ordinal: None,
            class: IngressShadowDecisionClass::Accepted,
            alias: IngressShadowAliasOutcome::None,
        });
        observe(IngressShadowObservation::Accepted {
            kind: IngressShadowRequestKind::Submit,
            stream_id: stream_id(),
        });
        observe(IngressShadowObservation::Decision {
            stream_id: stream_id(),
            claim_epoch: None,
            handled_ordinal: None,
            class: IngressShadowDecisionClass::Storage,
            alias: IngressShadowAliasOutcome::None,
        });

        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.admissions", &[])
                .unwrap_or(0),
            admissions_before + 2
        );
        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.completions", &[])
                .unwrap_or(0),
            completions_before + 2
        );
    }

    #[tokio::test]
    async fn enrollment_failure_is_not_a_submission_completion() {
        let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
        let completions_before = metrics
            .counter_sum("ingress.shadow.completions", &[])
            .unwrap_or(0);
        observe(IngressShadowObservation::Failed {
            kind: IngressShadowRequestKind::Enroll,
            stream_id: stream_id(),
            claim_epoch: None,
            handled_ordinal: None,
        });

        assert_eq!(
            metrics
                .counter_sum("ingress.shadow.completions", &[])
                .unwrap_or(0),
            completions_before,
            "enrollment failure must not count as a submission completion"
        );
    }
}
