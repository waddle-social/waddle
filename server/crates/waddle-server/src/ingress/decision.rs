//! Committed responsibility and the bounded post-commit work it authorizes.
use crate::{
    ingress_substrate::EffectReceiptKind,
    ingress_uow::ReconcileVerdict,
    server::routes::interpret::effects::{
        AppliedDurableEffects, ExternalEffect, PlanEffectDependency,
    },
};
use jid::BareJid;
use waddle_xmpp::ingress::{IngressOrdinal, MessageKey};
pub use waddle_xmpp::telemetry::attributes::IngressDecisionClass;
use waddle_xmpp_core::xep0359::StanzaId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AliasOutcomeClass {
    Inserted,
    NoOrigin,
    Existing,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectReceiptKey {
    pub kind: EffectReceiptKind,
    pub semantic_identity_hash: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct IngressDecision {
    pub class: IngressDecisionClass,
    pub message_key: Option<MessageKey>,
    pub ordinal: Option<IngressOrdinal>,
    pub alias: AliasOutcomeClass,
    pub verdict: Option<ReconcileVerdict>,
    pub archive_ids: Vec<(BareJid, StanzaId)>,
    pub applied_durable: std::sync::Arc<AppliedDurableEffects>,
    pub external: Vec<ExternalEffect>,
    /// Captured dependencies aligned with external effects.
    pub external_dependencies: Vec<Vec<PlanEffectDependency>>,
    /// Receipt identities fulfilled by each external effect, in the same order.
    pub external_receipts: Vec<Vec<EffectReceiptKey>>,
    pub receipts_pending: Vec<EffectReceiptKey>,
}

#[cfg(test)]
mod tests {
    use super::IngressDecisionClass;
    #[test]
    fn decision_matrix_advances_only_committed_classes() {
        use IngressDecisionClass::*;
        for class in [
            Accepted,
            ExistingCommitted,
            ExistingConsistent,
            ExistingRepaired,
            ExistingDivergent,
            OwnerFirstAcceptance,
            OwnerDuplicate,
            AliasConflict,
            SemanticMalformed,
            AuthorizationDenied,
            PolicyDenied,
            CaptureOverflow,
        ] {
            assert!(class.advances(), "{class:?}");
        }
        for class in [
            PrincipalMissing,
            ClaimFenceMissing,
            RoomGenerationStale,
            FrontierStale,
            SmOrdinalConflict,
            IntentContradiction,
            Storage,
            SerializationExhaustion,
            Timeout,
            AmbiguousCommit,
            Lineage,
            EpochUnsupported,
        ] {
            assert!(!class.advances(), "{class:?}");
        }
    }
}
