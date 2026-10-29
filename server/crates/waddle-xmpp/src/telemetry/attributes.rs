//! The metric-attribute allowlist (the cardinality budget's teeth).
//!
//! Every metric attribute is a closed enum here implementing the
//! sealed [`MetricAttribute`] trait. The macros in
//! [`crate::telemetry`] accept only these types, so an unbounded
//! value — a JID, a room JID, a stream id, a message id, user input —
//! cannot become a metric attribute by construction. Adding a new
//! attribute key or value means editing this file, where review can
//! hold the cardinality line.
//! [`crate::metrics`] is a documented pre-existing exception pending migration under #1330.

use opentelemetry::KeyValue;

use crate::ingress::IngressEffectKind;

mod sealed {
    pub trait Sealed {}
}

/// `condition` — the RFC 6120 §8.3.3 defined stanza-error conditions,
/// reusing the crate's existing typed enum (its `as_str()` already
/// yields the hyphenated wire names). Re-exported here so metric call
/// sites can name it through the allowlist module.
pub use crate::error::StanzaErrorCondition;

#[cfg(test)]
mod tests {
    use super::MetricAttribute;
    use crate::ingress::{IngressEffectIntent, IngressEffectKind};

    #[test]
    fn ingress_effect_kind_labels_match_storage_kind_names() {
        assert_eq!(
            IngressEffectKind::ALL.len(),
            IngressEffectIntent::storage_kind_names().len()
        );
        for kind in IngressEffectKind::ALL {
            assert_eq!(kind.key(), "kind");
            assert_eq!(
                IngressEffectIntent::storage_kind_names()
                    .iter()
                    .find(|(tag, _)| *tag == kind.storage_tag())
                    .map(|(_, name)| *name),
                Some(kind.value()),
                "{kind:?}",
            );
        }
    }
}

mod implementation;
pub use implementation::AdminOp;
pub use implementation::AuthErrorCode;
pub use implementation::AuthStage;
pub use implementation::CallControlRateLimitedSurface;
pub use implementation::CallSetupFailureReason;
pub use implementation::CallSignalEvent;
pub use implementation::ClaimOp;
pub use implementation::ClaimResult;
pub use implementation::FenceResult;
pub use implementation::HttpRouteTemplate;
pub use implementation::HttpStatusClass;
pub use implementation::IngressAliasOutcome;
pub use implementation::IngressAppendAuthorizationFailure;
pub use implementation::IngressDecisionClass;
pub use implementation::IngressEffectExecutionPhase;
pub use implementation::IngressGcOutcome;
pub use implementation::IngressMaintenanceOutcome;
pub use implementation::IngressMaintenancePhase;
pub use implementation::IngressReachabilityBlockReason;
pub use implementation::IngressUnrecoverableReason;
pub use implementation::IngressUnresolvedEffectKind;
pub use implementation::Janitor;
pub use implementation::MentionAudienceCheck;
pub use implementation::MessageKind;
pub use implementation::MetricAttribute;
pub use implementation::MucJoinDenyReason;
pub use implementation::ObserverHistoryTable;
pub use implementation::PushDisposition;
pub use implementation::PushFailureClass;
pub use implementation::PushProvider;
pub use implementation::PushQueue;
pub use implementation::PushRetryReason;
pub use implementation::PushStage;
pub use implementation::PushSuppressReason;
pub use implementation::RemoteOwnerMirrorOutcome;
pub use implementation::RemoteResourceUpdateOutcome;
pub use implementation::RequestOutcome;
pub use implementation::SessionInitFailureReason;
pub use implementation::SfuDenialReason;
pub use implementation::SmAckOutcome;
pub use implementation::SmDetachedAppendRejectionReason;
pub use implementation::SmEvictionPath;
pub use implementation::SmResumeOutcome;
pub use implementation::SweepOutcome;
pub use implementation::WebhookEventType;
pub use implementation::WebhookOutcome;
