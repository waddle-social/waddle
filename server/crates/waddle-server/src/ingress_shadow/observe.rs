use waddle_xmpp::ingress::IngressOrdinal;
use waddle_xmpp::ownership::ClaimEpoch;
use waddle_xmpp::pending_delivery::SmSessionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressShadowDecisionClass {
    Accepted,
    ExistingSameDigest,
    AliasConflict,
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
        }
    }
}
